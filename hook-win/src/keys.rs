//! The physical state of keys and mouse buttons on Windows, for the key rebinding's safety net
//! ([`reminedog_agent::os::Os::input_up`]).

use reminedog_core::{InputId, win_scancode_of};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW, VK_DECIMAL, VK_LBUTTON, VK_MBUTTON,
    VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD1, VK_PAUSE, VK_RBUTTON, VK_XBUTTON1, VK_XBUTTON2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};

/// Whether Windows reports the key or mouse button as up now; false when it cannot tell.
pub fn is_up(id: InputId) -> bool {
    let keys = virtual_keys(id);
    !keys.is_empty()
        && keys.iter().all(|&vk| {
            // SAFETY: plain query; the high bit is set while the key is down.
            let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
            state >= 0
        })
}

/// The virtual keys that report `id` held (any of them down: held). Empty when it cannot be
/// told: keys without a Win32 scancode.
fn virtual_keys(id: InputId) -> Vec<u16> {
    match id {
        InputId::Mouse(button) => button_virtual_key(button, buttons_swapped())
            .into_iter()
            .collect(),
        // GLFW 3.4 reports Pause as 0x45, which is Num Lock's scancode to Windows.
        InputId::Key(72) => vec![VK_PAUSE],
        InputId::Key(83) => vec![VK_NUMLOCK],
        // The keypad's digits and point are the navigation keys while Num Lock is off.
        InputId::Key(sc @ 89..=97) => [VK_NUMPAD1 + (sc - 89)]
            .into_iter()
            .chain(scancode_virtual_key(id))
            .collect(),
        InputId::Key(98) => [VK_NUMPAD0]
            .into_iter()
            .chain(scancode_virtual_key(id))
            .collect(),
        InputId::Key(99) => [VK_DECIMAL]
            .into_iter()
            .chain(scancode_virtual_key(id))
            .collect(),
        InputId::Key(_) => scancode_virtual_key(id).into_iter().collect(),
    }
}

/// The virtual key of a mouse button (SDL's numbering, which follows the buttons' logical
/// roles). GetAsyncKeyState reads the physical buttons: with the buttons `swapped`, the
/// logical left button is the physical right one.
fn button_virtual_key(button: u8, swapped: bool) -> Option<u16> {
    match (button, swapped) {
        (1, false) | (3, true) => Some(VK_LBUTTON),
        (1, true) | (3, false) => Some(VK_RBUTTON),
        (2, _) => Some(VK_MBUTTON),
        (4, _) => Some(VK_XBUTTON1),
        (5, _) => Some(VK_XBUTTON2),
        _ => None,
    }
}

/// Whether the user swapped the left and right mouse buttons in Windows' settings.
fn buttons_swapped() -> bool {
    // SAFETY: plain query.
    unsafe { GetSystemMetrics(SM_SWAPBUTTON) != 0 }
}

/// The virtual key Windows' keyboard layout gives the key's scancode (the left and right
/// modifiers apart).
fn scancode_virtual_key(id: InputId) -> Option<u16> {
    let scancode = u32::from(win_scancode_of(id)?);
    // GLFW's 0x100 for the 0xE0 prefix.
    let code = if scancode & 0x100 != 0 {
        0xE000 | (scancode & 0xFF)
    } else {
        scancode
    };
    // SAFETY: plain query.
    let vk = unsafe { MapVirtualKeyW(code, MAPVK_VSC_TO_VK_EX) };
    u16::try_from(vk).ok().filter(|&vk| vk != 0)
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_F3, VK_LSHIFT, VK_NUMPAD8, VK_RCONTROL, VK_RMENU,
    };

    use super::*;

    const F3: InputId = InputId::Key(60);
    const MOUSE4: InputId = InputId::Mouse(4);

    #[test]
    fn virtual_keys_of_keys_and_buttons() {
        // Keys whose virtual key does not depend on the keyboard layout.
        assert_eq!(virtual_keys(InputId::Key(225)), [VK_LSHIFT]);
        assert_eq!(virtual_keys(InputId::Key(228)), [VK_RCONTROL]);
        assert_eq!(virtual_keys(InputId::Key(230)), [VK_RMENU]);
        assert_eq!(virtual_keys(F3), [VK_F3]);
        assert_eq!(virtual_keys(InputId::Key(72)), [VK_PAUSE]);
        assert_eq!(virtual_keys(InputId::Key(83)), [VK_NUMLOCK]);
        // Keypad 8: also ↑ while Num Lock is off.
        assert_eq!(virtual_keys(InputId::Key(96))[0], VK_NUMPAD8);
        assert_eq!(virtual_keys(InputId::Key(96)).len(), 2);
        assert_eq!(virtual_keys(InputId::Mouse(2)), [VK_MBUTTON]);
        assert_eq!(virtual_keys(MOUSE4), [VK_XBUTTON1]);
        assert_eq!(virtual_keys(InputId::Mouse(5)), [VK_XBUTTON2]);
        // The left and right buttons by their roles: Windows reads the physical ones.
        assert_eq!(button_virtual_key(1, false), Some(VK_LBUTTON));
        assert_eq!(button_virtual_key(3, false), Some(VK_RBUTTON));
        assert_eq!(button_virtual_key(1, true), Some(VK_RBUTTON));
        assert_eq!(button_virtual_key(3, true), Some(VK_LBUTTON));
        assert_eq!(button_virtual_key(2, true), Some(VK_MBUTTON));
        assert_eq!(button_virtual_key(6, false), None);
        assert_eq!(virtual_keys(InputId::Mouse(1)).len(), 1);
        // Cannot tell: never reported as up.
        assert!(virtual_keys(InputId::Mouse(0)).is_empty());
        assert!(!is_up(InputId::Mouse(0)));
        // SDL-only keys have no Win32 scancode.
        assert!(virtual_keys(InputId::Key(300)).is_empty());
    }
}
