// HID report IDs shared by the report descriptor and the forwarder.
#pragma once
#define RID_KB    1   // boot-keyboard report: [modifier, reserved, key[6]] (8 bytes)
#define RID_MOUSE 2   // relative mouse: [buttons, dx, dy, wheel, pan] (5 bytes)
#define RID_ABS   3   // absolute mouse: [buttons, x:u16, y:u16, wheel, pan] (7 bytes)
