//! 捕まえたマウスの上のオーバーレイの自前のカーソルのための、Windows のポインターの設定。

use std::ffi::c_void;
use std::ptr;

use reminedog_render::{PointerSpeed, WINDOWS_DEFAULT_CURVE, parse_windows_curve};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SPI_GETMOUSE, SPI_GETMOUSESPEED, SystemParametersInfoW,
};

use crate::ffi::wide;

/// Windows のマウスの設定のポインターの速度と加速。ゲームが動いている間の変更も効くように、
/// 呼ぶたびに読み直す。
pub fn system_speed() -> PointerSpeed {
    let mut speed: i32 = 10;
    // しきい値 1、しきい値 2、加速（「ポインターの精度を高める」）。
    let mut mouse = [0i32; 3];
    // SAFETY: どちらの操作も、文書どおりの型をポインタを通して書く。
    unsafe {
        if SystemParametersInfoW(SPI_GETMOUSESPEED, 0, (&raw mut speed).cast::<c_void>(), 0) == 0 {
            speed = 10;
        }
        if SystemParametersInfoW(SPI_GETMOUSE, 0, mouse.as_mut_ptr().cast::<c_void>(), 0) == 0 {
            mouse = [0; 3];
        }
    }
    let enhance = mouse[2] != 0;
    let curve = if enhance {
        match (curve("SmoothMouseXCurve"), curve("SmoothMouseYCurve")) {
            (Some(x), Some(y)) => (x, y),
            _ => WINDOWS_DEFAULT_CURVE,
        }
    } else {
        WINDOWS_DEFAULT_CURVE
    };
    let result = PointerSpeed::windows(speed.clamp(1, 20) as u32, enhance, curve);
    log::debug!(
        "pointer speed {speed}, enhance pointer precision {}: {result:?}",
        if enhance { "on" } else { "off" }
    );
    result
}

/// `HKCU\Control Panel\Mouse` の加速の曲線。
fn curve(name: &str) -> Option<[f32; 5]> {
    let key = wide("Control Panel\\Mouse");
    let value = wide(name);
    let mut data = [0u8; 64];
    let mut len = data.len() as u32;
    // SAFETY: NUL で終わる名前。`len` はバッファの大きさを持ち、データの大きさを受け取る。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            ptr::null_mut(),
            data.as_mut_ptr().cast::<c_void>(),
            &mut len,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    parse_windows_curve(&data[..(len as usize).min(data.len())])
}
