//! Video/audio capture from the Kiwi's UVC/UAC interfaces via AVFoundation.
//!
//! Frames are delivered as NV12 full-range (`420f`) `CVPixelBuffer`s, which gpui's
//! `surface()` element draws directly on the GPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use block2::RcBlock;
use core_foundation::base::TCFType;
use core_video::pixel_buffer::{CVPixelBuffer, CVPixelBufferRef};
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureAudioPreviewOutput, AVCaptureConnection, AVCaptureDevice,
    AVCaptureDeviceFormat, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaType,
    AVMediaTypeAudio, AVMediaTypeVideo,
};
use objc2_core_media::{CMSampleBuffer, CMVideoFormatDescriptionGetDimensions};
use objc2_foundation::{NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};
use parking_lot::Mutex;

/// MacroSilicon MS2130 capture chip inside the Kiwi.
const VIDEO_VENDOR_TAG: &str = "VendorID_13407";
/// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange` ('420f'), the format gpui surfaces accept.
const PIXEL_FORMAT_420F: u32 = 0x3432_3066;

/// A captured frame. CVPixelBuffers are reference-counted and safe to share across threads.
#[derive(Clone)]
pub struct Frame(pub CVPixelBuffer, pub std::time::Instant);
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

pub fn latency_logging() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("KIWI_LATENCY").is_some())
}

#[derive(Default)]
struct LatencyStats {
    capture: Vec<f64>,
    display: Vec<f64>,
    since: Option<std::time::Instant>,
}

pub struct FrameSink {
    latest: Mutex<Option<Frame>>,
    stats: Mutex<LatencyStats>,
    pending: AtomicBool,
    pub frames: AtomicU64,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl FrameSink {
    /// `notify` is called (from the capture queue) when a new frame is ready and the
    /// previous one has been taken, so the UI is never flooded.
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            latest: Mutex::new(None),
            stats: Mutex::new(LatencyStats::default()),
            pending: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            notify: Box::new(notify),
        })
    }

    fn push(&self, frame: Frame) {
        *self.latest.lock() = Some(frame);
        self.frames.fetch_add(1, Ordering::Relaxed);
        if !self.pending.swap(true, Ordering::AcqRel) {
            (self.notify)();
        }
    }

    fn record_capture_delay(&self, ms: f64) {
        // The first frame after start can carry a stale timestamp; ignore nonsense values.
        if !(0.0..1000.0).contains(&ms) {
            return;
        }
        self.stats.lock().capture.push(ms);
    }

    /// Called when a frame is handed to the renderer.
    pub fn record_display(&self, frame: &Frame) {
        if !latency_logging() {
            return;
        }
        let mut st = self.stats.lock();
        st.display.push(frame.1.elapsed().as_secs_f64() * 1000.0);
        let since = *st.since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed().as_secs() >= 2 {
            let summary = |v: &mut Vec<f64>| {
                if v.is_empty() {
                    return "n/a".to_string();
                }
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let avg = v.iter().sum::<f64>() / v.len() as f64;
                format!("avg {avg:.1} ms, p50 {:.1}, max {:.1} (n={})", v[v.len() / 2], v[v.len() - 1], v.len())
            };
            let cap = summary(&mut st.capture);
            let disp = summary(&mut st.display);
            log::info!("latency: AVFoundation delivery {cap} | delivery→render {disp}");
            st.capture.clear();
            st.display.clear();
            st.since = Some(std::time::Instant::now());
        }
    }

    pub fn take(&self) -> Option<Frame> {
        self.pending.store(false, Ordering::Release);
        self.latest.lock().take()
    }
}

pub struct DelegateIvars {
    sink: Arc<FrameSink>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "KiwiVideoDelegate"]
    #[ivars = DelegateIvars]
    pub struct VideoDelegate;

    unsafe impl NSObjectProtocol for VideoDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for VideoDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            let Some(image) = (unsafe { sample.image_buffer() }) else { return };
            let raw = objc2_core_foundation::CFRetained::as_ptr(&image).as_ptr() as CVPixelBufferRef;
            // Retains: the frame outlives the sample buffer.
            let pb = unsafe { CVPixelBuffer::wrap_under_get_rule(raw) };
            if latency_logging() {
                // How old the frame already is when AVFoundation hands it to us.
                let pts = unsafe { sample.presentation_time_stamp().seconds() };
                let now = unsafe { objc2_core_media::CMClock::host_time_clock().time().seconds() };
                self.ivars().sink.record_capture_delay((now - pts) * 1000.0);
            }
            self.ivars().sink.push(Frame(pb, std::time::Instant::now()));
        }
    }
);

impl VideoDelegate {
    fn new(sink: Arc<FrameSink>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { sink });
        unsafe { msg_send![super(this), init] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    Pending,
}

fn media(kind: &'static Option<&'static AVMediaType>) -> &'static AVMediaType {
    kind.expect("AVFoundation media type constant")
}

/// Check (and if needed request) camera/microphone permission. `on_decided` runs on an
/// arbitrary thread once the user answers a prompt.
pub fn ensure_access(audio: bool, on_decided: impl Fn(bool) + Send + Sync + 'static) -> Access {
    let ty = if audio { media(unsafe { &AVMediaTypeAudio }) } else { media(unsafe { &AVMediaTypeVideo }) };
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(ty) };
    match status {
        AVAuthorizationStatus::Authorized => Access::Granted,
        AVAuthorizationStatus::NotDetermined => {
            let block = RcBlock::new(move |granted: Bool| on_decided(granted.as_bool()));
            unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(ty, &block) };
            Access::Pending
        }
        _ => Access::Denied,
    }
}

fn find_device(kind: &'static AVMediaType) -> Option<Retained<AVCaptureDevice>> {
    #[allow(deprecated)]
    let devices = unsafe { AVCaptureDevice::devicesWithMediaType(kind) };
    devices.iter().find(|d| {
        let name = unsafe { d.localizedName() }.to_string();
        let model = unsafe { d.modelID() }.to_string();
        log::debug!("capture device: {name:?} model {model:?}");
        name.contains("Kiwi") || model.contains(VIDEO_VENDOR_TAG)
    })
}

pub fn kiwi_video_present() -> bool {
    find_device(media(unsafe { &AVMediaTypeVideo })).is_some()
}

/// One capture mode the device offers.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoMode {
    pub width: i32,
    pub height: i32,
    pub fps: f64,
    /// CoreMedia media subtype (FourCC), e.g. 'dmb1' (MJPEG) or 'yuvs' (YUY2).
    pub fourcc: u32,
}

fn fourcc_str(v: u32) -> String {
    v.to_be_bytes().iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' }).collect()
}

impl VideoMode {
    pub fn compressed(&self) -> bool {
        matches!(&fourcc_str(self.fourcc)[..], "dmb1" | "jpeg" | "mjpg" | "MJPG" | "avc1" | "hvc1")
    }

    pub fn codec(&self) -> String {
        match &fourcc_str(self.fourcc)[..] {
            "dmb1" | "jpeg" | "mjpg" | "MJPG" => "MJPEG".into(),
            "yuvs" | "2vuy" | "yuv2" | "YUY2" => "YUY2".into(),
            "420v" | "420f" | "NV12" => "NV12".into(),
            other => other.into(),
        }
    }

    pub fn label(&self) -> String {
        format!("{}×{} @ {:.0} · {}", self.width, self.height, self.fps, self.codec())
    }

    pub fn fourcc(&self) -> String {
        fourcc_str(self.fourcc)
    }
}

struct FormatChoice {
    format: Retained<AVCaptureDeviceFormat>,
    min_duration: objc2_core_media::CMTime,
    mode: VideoMode,
}

fn all_choices(device: &AVCaptureDevice) -> Vec<FormatChoice> {
    let mut out = Vec::new();
    for format in unsafe { device.formats() }.iter() {
        let desc = unsafe { format.formatDescription() };
        let dims = unsafe { CMVideoFormatDescriptionGetDimensions(&desc) };
        let fourcc = unsafe { desc.media_sub_type() };
        for range in unsafe { format.videoSupportedFrameRateRanges() }.iter() {
            out.push(FormatChoice {
                format: format.clone(),
                min_duration: unsafe { range.minFrameDuration() },
                mode: VideoMode { width: dims.width, height: dims.height, fps: unsafe { range.maxFrameRate() }, fourcc },
            });
        }
    }
    out
}

/// Every distinct mode, largest and fastest first.
pub fn list_modes() -> Vec<VideoMode> {
    let Some(device) = find_device(media(unsafe { &AVMediaTypeVideo })) else { return Vec::new() };
    let mut modes: Vec<VideoMode> = all_choices(&device).into_iter().map(|c| c.mode).collect();
    modes.sort_by(|a, b| {
        (b.width * b.height, (b.fps * 100.0) as i64, !b.compressed())
            .cmp(&(a.width * a.height, (a.fps * 100.0) as i64, !a.compressed()))
    });
    modes.dedup();
    modes
}

/// The requested mode if available, otherwise 1920x1080 (or the largest) at the highest rate.
fn choose_format(device: &AVCaptureDevice, want: Option<&VideoMode>) -> Option<FormatChoice> {
    let choices = all_choices(device);
    if let Some(want) = want {
        if let Some(i) = choices.iter().position(|c| {
            c.mode.width == want.width
                && c.mode.height == want.height
                && c.mode.fourcc == want.fourcc
                && (c.mode.fps - want.fps).abs() < 0.5
        }) {
            return choices.into_iter().nth(i);
        }
        log::warn!("requested video mode {} not available; using default", want.label());
    }
    let score = |c: &FormatChoice| {
        let m = &c.mode;
        (
            (m.width, m.height) == (1920, 1080),
            m.width as i64 * m.height as i64,
            (m.fps * 100.0) as i64,
            m.codec() == "NV12",
        )
    };
    choices.into_iter().max_by_key(score)
}

pub struct Capture {
    session: Retained<AVCaptureSession>,
    _delegate: Retained<VideoDelegate>,
    _queue: DispatchRetained<DispatchQueue>,
    audio_out: Option<Retained<AVCaptureAudioPreviewOutput>>,
    pub description: String,
    pub mode: Option<VideoMode>,
}

impl Capture {
    pub fn start(sink: Arc<FrameSink>, with_audio: bool, want: Option<&VideoMode>) -> Result<Self> {
        let video_ty = media(unsafe { &AVMediaTypeVideo });
        let device = find_device(video_ty).ok_or_else(|| anyhow!("Kiwi capture device not found"))?;
        let name = unsafe { device.localizedName() }.to_string();

        let session = unsafe { AVCaptureSession::new() };
        unsafe { session.beginConfiguration() };

        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|e| anyhow!("open video input: {}", e.localizedDescription()))?;
        if !unsafe { session.canAddInput(&input) } {
            unsafe { session.commitConfiguration() };
            return Err(anyhow!("cannot add video input"));
        }
        unsafe { session.addInput(&input) };

        let output = unsafe { AVCaptureVideoDataOutput::new() };
        let key = NSString::from_str("PixelFormatType");
        let value = NSNumber::new_u32(PIXEL_FORMAT_420F);
        let settings: Retained<NSDictionary<NSString, AnyObject>> =
            NSDictionary::from_slices(&[&*key], &[&*value as &AnyObject]);
        unsafe {
            output.setVideoSettings(Some(&settings));
            output.setAlwaysDiscardsLateVideoFrames(true);
        }
        let delegate = VideoDelegate::new(sink);
        let queue = DispatchQueue::new("kiwi.video", None);
        unsafe {
            output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&queue));
        }
        if !unsafe { session.canAddOutput(&output) } {
            unsafe { session.commitConfiguration() };
            return Err(anyhow!("cannot add video output"));
        }
        unsafe { session.addOutput(&output) };

        let mut description = name.clone();
        let mut mode = None;
        // Hold the configuration lock until the session is running: otherwise starting the
        // session can reset the frame rate (we measured 20 fps delivered for a 60 fps mode).
        let choice = choose_format(&device, want);
        let locked = choice.is_some() && unsafe { device.lockForConfiguration() }.is_ok();
        if let (Some(choice), true) = (&choice, locked) {
            unsafe {
                device.setActiveFormat(&choice.format);
                device.setActiveVideoMinFrameDuration(choice.min_duration);
                device.setActiveVideoMaxFrameDuration(choice.min_duration);
            }
            description = format!("{name} · {}", choice.mode.label());
            mode = Some(choice.mode.clone());
        }

        let audio_out = if with_audio { add_audio(&session) } else { None };
        unsafe { session.commitConfiguration() };
        unsafe { session.startRunning() };
        if let (Some(choice), true) = (&choice, locked) {
            unsafe {
                if !std::ptr::eq(&*device.activeFormat(), &*choice.format) {
                    device.setActiveFormat(&choice.format);
                }
                device.setActiveVideoMinFrameDuration(choice.min_duration);
                device.setActiveVideoMaxFrameDuration(choice.min_duration);
                device.unlockForConfiguration();
            }
        }
        unsafe {
            let fmt = device.activeFormat();
            let desc = fmt.formatDescription();
            let dims = CMVideoFormatDescriptionGetDimensions(&desc);
            let min = device.activeVideoMinFrameDuration().seconds();
            let max = device.activeVideoMaxFrameDuration().seconds();
            log::info!(
                "active format {}x{} [{}], frame duration {:.1}–{:.1} ms",
                dims.width,
                dims.height,
                fourcc_str(desc.media_sub_type()),
                min * 1000.0,
                max * 1000.0
            );
        }
        log::info!("video capture started: {description}");

        Ok(Self { session, _delegate: delegate, _queue: queue, audio_out, description, mode })
    }

    pub fn has_audio(&self) -> bool {
        self.audio_out.is_some()
    }

    pub fn set_volume(&self, volume: f32) {
        if let Some(out) = &self.audio_out {
            unsafe { out.setVolume(volume) };
        }
    }
}

fn add_audio(session: &AVCaptureSession) -> Option<Retained<AVCaptureAudioPreviewOutput>> {
    let audio_ty = media(unsafe { &AVMediaTypeAudio });
    let device = find_device(audio_ty)?;
    let input = match unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) } {
        Ok(i) => i,
        Err(e) => {
            log::warn!("audio input unavailable: {}", e.localizedDescription());
            return None;
        }
    };
    if !unsafe { session.canAddInput(&input) } {
        return None;
    }
    unsafe { session.addInput(&input) };
    let out = unsafe { AVCaptureAudioPreviewOutput::new() };
    unsafe { out.setVolume(1.0) };
    if !unsafe { session.canAddOutput(&out) } {
        return None;
    }
    unsafe { session.addOutput(&out) };
    Some(out)
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe { self.session.stopRunning() };
    }
}
