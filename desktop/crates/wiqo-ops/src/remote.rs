//! The live remote screen's two hands on this computer (wiqo-agent `remote_screen`): a fast picture of the main display, made small and encoded
//! as JPEG in one step, and the phone's touches and keys put straight into the system's input queue. Nothing here spawns a process: a frame or a
//! click costs milliseconds, not the hundreds a helper program per call would.
//!
//! Coordinates come from the phone as fractions of the main display (0…1), so the phone never needs to know this screen's size or scale.

use anyhow::Result;
use image::{codecs::jpeg::JpegEncoder, ExtendedColorType};

/// One encoded frame and the size it was drawn at.
pub struct Frame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// A cheap fingerprint of the picture, to skip a frame that shows nothing new.
    pub digest: u64,
}

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

/// What the phone does on the screen. `x`, `y` are fractions of the main display.
#[derive(Clone, Debug)]
pub enum Input {
    Move { x: f64, y: f64 },
    Down { x: f64, y: f64, button: Button },
    Up { x: f64, y: f64, button: Button },
    Click { x: f64, y: f64, button: Button, count: u8 },
    Scroll { x: f64, y: f64, dx: i32, dy: i32 },
    Text(String),
    Key { key: String, modifiers: Vec<String> },
}

fn digest(bytes: &[u8]) -> u64 {
    // Every byte counts (one typed letter must change it), eight at a time: a few milliseconds for a full frame.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325 ^ bytes.len() as u64;
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_le_bytes(chunk.try_into().expect("eight bytes"));
        hash = (hash ^ word).wrapping_mul(0x0100_0000_01b3).rotate_left(29);
    }
    for byte in chunks.remainder() {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn encode_rgb(rgb: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity((width * height / 6) as usize);
    JpegEncoder::new_with_quality(&mut out, quality.clamp(20, 95)).encode(rgb, width, height, ExtendedColorType::Rgb8)?;
    Ok(out)
}

/// The main display, drawn at most `max_width` wide and encoded at `quality` (20…95). `previous`: the last frame's digest; a picture with the same
/// digest is not encoded again (None comes back).
pub fn capture(max_width: u32, quality: u8, previous: Option<u64>) -> Result<Option<Frame>> {
    let (rgb, width, height) = platform::grab_rgb(max_width)?;
    let digest = digest(&rgb);
    if previous == Some(digest) {
        return Ok(None);
    }
    let jpeg = encode_rgb(&rgb, width, height, quality)?;
    Ok(Some(Frame { jpeg, width, height, digest }))
}

/// The main display's size in the units input uses (points on macOS, pixels on Windows).
pub fn display_size() -> (f64, f64) {
    platform::display_size()
}

/// Puts one input into the system, as if the person did it here.
pub fn inject(input: &Input) -> Result<()> {
    platform::inject(input)
}

/// Whether this computer lets Wiqo move the pointer and type (macOS: Accessibility). None where the system cannot say.
pub fn input_allowed() -> Option<bool> {
    platform::input_allowed()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Button, Input};
    use anyhow::{anyhow, Result};
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::context::{CGContext, CGInterpolationQuality};
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, ScrollEventUnit, EventField, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    pub fn input_allowed() -> Option<bool> {
        // SAFETY: a plain query of this process's Accessibility permission.
        Some(unsafe { AXIsProcessTrusted() })
    }

    pub fn display_size() -> (f64, f64) {
        let bounds = CGDisplay::main().bounds();
        (bounds.size.width, bounds.size.height)
    }

    /// The display's picture drawn straight into a small RGBA buffer by the system (no full-size copy, no resize pass in Rust), then made RGB.
    pub fn grab_rgb(max_width: u32) -> Result<(Vec<u8>, u32, u32)> {
        let display = CGDisplay::main();
        let image = display.image().ok_or_else(|| anyhow!("SCREEN_CAPTURE_FAILED"))?;
        let (source_width, source_height) = (image.width() as f64, image.height() as f64);
        if source_width < 1.0 || source_height < 1.0 {
            return Err(anyhow!("SCREEN_CAPTURE_FAILED"));
        }
        let scale = (f64::from(max_width) / source_width).min(1.0);
        let width = ((source_width * scale).round() as usize).max(2) & !1;
        let height = ((source_height * scale).round() as usize).max(2) & !1;
        let space = CGColorSpace::create_device_rgb();
        let mut context = CGContext::create_bitmap_context(None, width, height, 8, width * 4, &space,
            core_graphics::base::kCGImageAlphaNoneSkipLast | core_graphics::base::kCGBitmapByteOrder32Big);
        context.set_interpolation_quality(CGInterpolationQuality::CGInterpolationQualityMedium);
        context.draw_image(CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(width as f64, height as f64)), &image);
        let row = context.bytes_per_row();
        let data = context.data();
        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            let line = &data[y * row..y * row + width * 4];
            for pixel in line.chunks_exact(4) {
                rgb.extend_from_slice(&pixel[..3]);
            }
        }
        Ok((rgb, width as u32, height as u32))
    }

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| anyhow!("INPUT_FAILED"))
    }

    fn point(x: f64, y: f64) -> CGPoint {
        let bounds = CGDisplay::main().bounds();
        CGPoint::new(bounds.origin.x + x.clamp(0.0, 1.0) * bounds.size.width, bounds.origin.y + y.clamp(0.0, 1.0) * bounds.size.height)
    }

    fn mouse(kind: CGEventType, at: CGPoint, button: CGMouseButton, clicks: i64) -> Result<()> {
        let event = CGEvent::new_mouse_event(source()?, kind, at, button).map_err(|_| anyhow!("INPUT_FAILED"))?;
        if clicks > 0 {
            event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, clicks);
        }
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn kinds(button: Button) -> (CGEventType, CGEventType, CGMouseButton) {
        match button {
            Button::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp, CGMouseButton::Left),
            Button::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp, CGMouseButton::Right),
        }
    }

    /// The virtual key codes of the keys the phone sends by name (US layout positions; letters are typed as text instead).
    fn key_code(name: &str) -> Option<CGKeyCode> {
        Some(match name {
            "enter" | "return" => 36,
            "tab" => 48,
            "space" => 49,
            "backspace" => 51,
            "escape" => 53,
            "delete" => 117,
            "home" => 115,
            "end" => 119,
            "pageup" => 116,
            "pagedown" => 121,
            "left" => 123,
            "right" => 124,
            "down" => 125,
            "up" => 126,
            "f5" => 96,
            "a" => 0, "c" => 8, "v" => 9, "x" => 7, "z" => 6, "s" => 1, "f" => 3, "t" => 17, "w" => 13, "r" => 15, "n" => 45, "l" => 37, "q" => 12,
            _ => return None,
        })
    }

    fn flags(modifiers: &[String]) -> CGEventFlags {
        let mut flags = CGEventFlags::empty();
        for modifier in modifiers {
            flags |= match modifier.as_str() {
                // The phone's "Ctrl" means the shortcut key of this system: Command on a Mac.
                "cmd" | "meta" | "ctrl" => CGEventFlags::CGEventFlagCommand,
                "control" => CGEventFlags::CGEventFlagControl,
                "alt" | "option" => CGEventFlags::CGEventFlagAlternate,
                "shift" => CGEventFlags::CGEventFlagShift,
                _ => CGEventFlags::empty(),
            };
        }
        flags
    }

    pub fn inject(input: &Input) -> Result<()> {
        match input {
            Input::Move { x, y } => mouse(CGEventType::MouseMoved, point(*x, *y), CGMouseButton::Left, 0),
            Input::Down { x, y, button } => {
                let (down, _, which) = kinds(*button);
                mouse(down, point(*x, *y), which, 1)
            }
            Input::Up { x, y, button } => {
                let (_, up, which) = kinds(*button);
                mouse(up, point(*x, *y), which, 1)
            }
            Input::Click { x, y, button, count } => {
                let at = point(*x, *y);
                let (down, up, which) = kinds(*button);
                mouse(CGEventType::MouseMoved, at, CGMouseButton::Left, 0)?;
                for click in 1..=i64::from((*count).clamp(1, 3)) {
                    mouse(down, at, which, click)?;
                    mouse(up, at, which, click)?;
                }
                Ok(())
            }
            Input::Scroll { x, y, dx, dy } => {
                mouse(CGEventType::MouseMoved, point(*x, *y), CGMouseButton::Left, 0)?;
                let event = CGEvent::new_scroll_event(source()?, ScrollEventUnit::PIXEL, 2, (*dy).clamp(-2000, 2000), (*dx).clamp(-2000, 2000), 0)
                    .map_err(|_| anyhow!("INPUT_FAILED"))?;
                event.post(CGEventTapLocation::HID);
                Ok(())
            }
            Input::Text(text) => {
                // Text goes in as Unicode, a few characters per event, so any language and emoji arrive as written.
                let units: Vec<u16> = text.encode_utf16().collect();
                for chunk in units.chunks(16) {
                    for down in [true, false] {
                        let event = CGEvent::new_keyboard_event(source()?, 0, down).map_err(|_| anyhow!("INPUT_FAILED"))?;
                        event.set_string_from_utf16_unchecked(chunk);
                        event.post(CGEventTapLocation::HID);
                    }
                }
                Ok(())
            }
            Input::Key { key, modifiers } => {
                let code = key_code(key).ok_or_else(|| anyhow!("UNKNOWN_KEY"))?;
                let flags = flags(modifiers);
                for down in [true, false] {
                    let event = CGEvent::new_keyboard_event(source()?, code, down).map_err(|_| anyhow!("INPUT_FAILED"))?;
                    event.set_flags(flags);
                    event.post(CGEventTapLocation::HID);
                }
                Ok(())
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{Button, Input};
    use anyhow::{anyhow, Result};
    use std::mem::size_of;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    pub fn input_allowed() -> Option<bool> {
        None
    }

    pub fn display_size() -> (f64, f64) {
        // SAFETY: plain metric queries.
        unsafe { (f64::from(GetSystemMetrics(SM_CXSCREEN)), f64::from(GetSystemMetrics(SM_CYSCREEN))) }
    }

    pub fn grab_rgb(max_width: u32) -> Result<(Vec<u8>, u32, u32)> {
        let monitors = xcap::Monitor::all().map_err(|_| anyhow!("SCREEN_CAPTURE_FAILED"))?;
        let monitor = monitors.iter().find(|monitor| monitor.is_primary().unwrap_or(false)).or_else(|| monitors.first()).ok_or_else(|| anyhow!("DISPLAY_NOT_FOUND"))?;
        let picture = monitor.capture_image().map_err(|_| anyhow!("SCREEN_CAPTURE_FAILED"))?;
        let scale = (f64::from(max_width) / f64::from(picture.width())).min(1.0);
        let width = ((f64::from(picture.width()) * scale).round() as u32).max(2) & !1;
        let height = ((f64::from(picture.height()) * scale).round() as u32).max(2) & !1;
        let small = if width == picture.width() { picture } else { image::imageops::resize(&picture, width, height, image::imageops::FilterType::Triangle) };
        let rgb = image::DynamicImage::ImageRgba8(small).into_rgb8().into_raw();
        Ok((rgb, width, height))
    }

    fn send(inputs: &[INPUT]) -> Result<()> {
        // SAFETY: the slice is a valid array of INPUT structures for its length.
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() { Ok(()) } else { Err(anyhow!("INPUT_FAILED")) }
    }

    fn mouse_input(flags: u32, x: f64, y: f64, data: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    // Absolute coordinates on the primary display are 0…65535: the phone's fractions map straight onto them.
                    dx: (x.clamp(0.0, 1.0) * 65_535.0) as i32,
                    dy: (y.clamp(0.0, 1.0) * 65_535.0) as i32,
                    mouseData: data as u32,
                    dwFlags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn key_input(vk: u16, scan: u16, flags: u32) -> INPUT {
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
    }

    fn virtual_key(name: &str) -> Option<u16> {
        Some(match name {
            "enter" | "return" => VK_RETURN,
            "tab" => VK_TAB,
            "space" => VK_SPACE,
            "backspace" => VK_BACK,
            "escape" => VK_ESCAPE,
            "delete" => VK_DELETE,
            "home" => VK_HOME,
            "end" => VK_END,
            "pageup" => VK_PRIOR,
            "pagedown" => VK_NEXT,
            "left" => VK_LEFT,
            "right" => VK_RIGHT,
            "down" => VK_DOWN,
            "up" => VK_UP,
            "f5" => VK_F5,
            single if single.len() == 1 && single.as_bytes()[0].is_ascii_lowercase() => u16::from(single.as_bytes()[0].to_ascii_uppercase()),
            _ => return None,
        })
    }

    fn modifier(name: &str) -> Option<u16> {
        Some(match name {
            "ctrl" | "control" => VK_CONTROL,
            "cmd" | "meta" => VK_LWIN,
            "alt" | "option" => VK_MENU,
            "shift" => VK_SHIFT,
            _ => return None,
        })
    }

    pub fn inject(input: &Input) -> Result<()> {
        let buttons = |button: Button| match button {
            Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        };
        match input {
            Input::Move { x, y } => send(&[mouse_input(0, *x, *y, 0)]),
            Input::Down { x, y, button } => send(&[mouse_input(buttons(*button).0, *x, *y, 0)]),
            Input::Up { x, y, button } => send(&[mouse_input(buttons(*button).1, *x, *y, 0)]),
            Input::Click { x, y, button, count } => {
                let (down, up) = buttons(*button);
                let mut inputs = vec![mouse_input(0, *x, *y, 0)];
                for _ in 0..(*count).clamp(1, 3) {
                    inputs.push(mouse_input(down, *x, *y, 0));
                    inputs.push(mouse_input(up, *x, *y, 0));
                }
                send(&inputs)
            }
            Input::Scroll { x, y, dx, dy } => {
                // Wheel units: 120 is one notch; the phone sends pixels, about a third of a notch each.
                let mut inputs = vec![mouse_input(0, *x, *y, 0)];
                if *dy != 0 { inputs.push(mouse_input(MOUSEEVENTF_WHEEL, *x, *y, (*dy * 3).clamp(-2400, 2400))); }
                if *dx != 0 { inputs.push(mouse_input(MOUSEEVENTF_HWHEEL, *x, *y, (-*dx * 3).clamp(-2400, 2400))); }
                send(&inputs)
            }
            Input::Text(text) => {
                let mut inputs = Vec::new();
                for unit in text.encode_utf16() {
                    inputs.push(key_input(0, unit, KEYEVENTF_UNICODE));
                    inputs.push(key_input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
                send(&inputs)
            }
            Input::Key { key, modifiers } => {
                let vk = virtual_key(key).ok_or_else(|| anyhow!("UNKNOWN_KEY"))?;
                let held: Vec<u16> = modifiers.iter().filter_map(|name| modifier(name)).collect();
                let mut inputs: Vec<INPUT> = held.iter().map(|code| key_input(*code, 0, 0)).collect();
                inputs.push(key_input(vk, 0, 0));
                inputs.push(key_input(vk, 0, KEYEVENTF_KEYUP));
                inputs.extend(held.iter().rev().map(|code| key_input(*code, 0, KEYEVENTF_KEYUP)));
                send(&inputs)
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::Input;
    use anyhow::{anyhow, Result};

    pub fn input_allowed() -> Option<bool> {
        None
    }

    pub fn display_size() -> (f64, f64) {
        (1920.0, 1080.0)
    }

    pub fn grab_rgb(max_width: u32) -> Result<(Vec<u8>, u32, u32)> {
        let monitors = xcap::Monitor::all().map_err(|_| anyhow!("SCREEN_CAPTURE_FAILED"))?;
        let monitor = monitors.first().ok_or_else(|| anyhow!("DISPLAY_NOT_FOUND"))?;
        let picture = monitor.capture_image().map_err(|_| anyhow!("SCREEN_CAPTURE_FAILED"))?;
        let scale = (f64::from(max_width) / f64::from(picture.width())).min(1.0);
        let width = ((f64::from(picture.width()) * scale).round() as u32).max(2) & !1;
        let height = ((f64::from(picture.height()) * scale).round() as u32).max(2) & !1;
        let small = image::imageops::resize(&picture, width, height, image::imageops::FilterType::Triangle);
        Ok((image::DynamicImage::ImageRgba8(small).into_rgb8().into_raw(), width, height))
    }

    pub fn inject(_input: &Input) -> Result<()> {
        Err(anyhow!("UNSUPPORTED_PLATFORM"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_picture_has_the_same_digest_and_a_changed_one_does_not() {
        let mut picture = vec![10u8; 300_000];
        let first = digest(&picture);
        assert_eq!(first, digest(&picture));
        picture[0] = 11;
        assert_ne!(first, digest(&picture));
    }

    #[test]
    fn rgb_is_encoded_as_jpeg() {
        let jpeg = encode_rgb(&vec![128u8; 64 * 32 * 3], 64, 32, 60).expect("encodes");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    }
}
