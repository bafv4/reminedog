//! Windows でのキーとマウスのボタンの物理の状態。キーの置き換えの安全網のため
//! （[`reminedog_agent::os::Os::input_up`]）。

use reminedog_core::{InputId, win_scancode_of};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW, VK_DECIMAL, VK_LBUTTON, VK_MBUTTON,
    VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD1, VK_PAUSE, VK_RBUTTON, VK_XBUTTON1, VK_XBUTTON2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};

/// Windows が今、キーかマウスのボタンを離していると答えるか。分からなければ false。
pub fn is_up(id: InputId) -> bool {
    let keys = virtual_keys(id);
    !keys.is_empty()
        && keys.iter().all(|&vk| {
            // SAFETY: ただの問い合わせ。キーが下りている間は上位のビットが立つ。
            let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
            state >= 0
        })
}

/// `id` を押していると答える仮想キー（どれかが下りていれば押している）。分からなければ空：
/// Win32 のスキャンコードのないキー。
fn virtual_keys(id: InputId) -> Vec<u16> {
    match id {
        InputId::Mouse(button) => button_virtual_key(button, buttons_swapped())
            .into_iter()
            .collect(),
        // GLFW 3.4 は Pause を 0x45 と報告し、それは Windows では Num Lock のスキャンコード。
        InputId::Key(72) => vec![VK_PAUSE],
        InputId::Key(83) => vec![VK_NUMLOCK],
        // Num Lock がオフの間、テンキーの数字と点は移動のキー。
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

/// マウスのボタンの仮想キー（SDL の番号の振り方で、ボタンの論理的な役割に従う）。
/// GetAsyncKeyState は物理のボタンを読む：ボタンを `swapped` したときは、論理的な左のボタンは
/// 物理の右のボタン。
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

/// 利用者が Windows の設定で左右のマウスのボタンを入れ替えたか。
fn buttons_swapped() -> bool {
    // SAFETY: ただの問い合わせ。
    unsafe { GetSystemMetrics(SM_SWAPBUTTON) != 0 }
}

/// Windows のキーボードの配列がキーのスキャンコードに与える仮想キー（左右の修飾キーは別々）。
fn scancode_virtual_key(id: InputId) -> Option<u16> {
    let scancode = u32::from(win_scancode_of(id)?);
    // GLFW の、0xE0 の前置きを表す 0x100。
    let code = if scancode & 0x100 != 0 {
        0xE000 | (scancode & 0xFF)
    } else {
        scancode
    };
    // SAFETY: ただの問い合わせ。
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
        // 仮想キーがキーボードの配列に左右されないキー。
        assert_eq!(virtual_keys(InputId::Key(225)), [VK_LSHIFT]);
        assert_eq!(virtual_keys(InputId::Key(228)), [VK_RCONTROL]);
        assert_eq!(virtual_keys(InputId::Key(230)), [VK_RMENU]);
        assert_eq!(virtual_keys(F3), [VK_F3]);
        assert_eq!(virtual_keys(InputId::Key(72)), [VK_PAUSE]);
        assert_eq!(virtual_keys(InputId::Key(83)), [VK_NUMLOCK]);
        // テンキーの 8：Num Lock がオフの間は ↑ でもある。
        assert_eq!(virtual_keys(InputId::Key(96))[0], VK_NUMPAD8);
        assert_eq!(virtual_keys(InputId::Key(96)).len(), 2);
        assert_eq!(virtual_keys(InputId::Mouse(2)), [VK_MBUTTON]);
        assert_eq!(virtual_keys(MOUSE4), [VK_XBUTTON1]);
        assert_eq!(virtual_keys(InputId::Mouse(5)), [VK_XBUTTON2]);
        // 役割による左右のボタン：Windows は物理のものを読む。
        assert_eq!(button_virtual_key(1, false), Some(VK_LBUTTON));
        assert_eq!(button_virtual_key(3, false), Some(VK_RBUTTON));
        assert_eq!(button_virtual_key(1, true), Some(VK_RBUTTON));
        assert_eq!(button_virtual_key(3, true), Some(VK_LBUTTON));
        assert_eq!(button_virtual_key(2, true), Some(VK_MBUTTON));
        assert_eq!(button_virtual_key(6, false), None);
        assert_eq!(virtual_keys(InputId::Mouse(1)).len(), 1);
        // 分からない：離しているとは答えない。
        assert!(virtual_keys(InputId::Mouse(0)).is_empty());
        assert!(!is_up(InputId::Mouse(0)));
        // SDL だけのキーには Win32 のスキャンコードがない。
        assert!(virtual_keys(InputId::Key(300)).is_empty());
    }
}
