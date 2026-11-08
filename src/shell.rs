//! Windows shell integration: open a file in its default app, and fetch the
//! shell's thumbnail for any file (used for HEIC/DNG/videos and as a fallback
//! when the `image` crate can't decode a file).

use std::path::Path;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP, HDC,
};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_RESIZETOFIT,
    SIIGBF_THUMBNAILONLY, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

/// Initializes COM (single-threaded apartment) on the current thread for the
/// guard's lifetime. If COM was already initialized (S_FALSE) the init is
/// still counted and is balanced by `CoUninitialize` on drop. If the thread is
/// already in a different apartment (RPC_E_CHANGED_MODE) nothing was counted,
/// so nothing is undone; shell calls still work from an MTA thread.
pub(crate) struct ComGuard {
    initialized: bool,
    // COM init is per-thread, so the guard must be dropped where it was made.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComGuard {
    pub(crate) fn new() -> ComGuard {
        // SAFETY: plain FFI call; the matching CoUninitialize is in Drop and
        // only runs when this call succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        ComGuard {
            initialized: hr.is_ok(),
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful CoInitializeEx in `new` on the
            // same thread (ComGuard is !Send via its PhantomData marker).
            unsafe { CoUninitialize() };
        }
    }
}

/// Opens `path` with its default application (ShellExecuteW "open").
pub fn open_default(path: &Path) -> std::io::Result<()> {
    let _com = ComGuard::new();
    let file = HSTRING::from(path);
    // SAFETY: all string arguments are valid NUL-terminated wide strings that
    // outlive the call; null parameters/directory are allowed.
    let result = unsafe { ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL) };
    let code = result.0 as isize;
    if code > 32 {
        return Ok(());
    }
    Err(match code {
        // These ShellExecute codes coincide with the Win32 error codes.
        2 | 3 | 5 | 8 | 11 => std::io::Error::from_raw_os_error(code as i32),
        31 => std::io::Error::other("no application is associated with this file type"),
        _ => std::io::Error::other(format!("ShellExecuteW failed with code {code}")),
    })
}

/// Returns the Windows shell's thumbnail for `path`, scaled to fit a
/// `size`×`size` box (with SIIGBF_BIGGERSIZEOK the shell may hand back a
/// larger cached image, so callers that need a hard cap must enforce it).
/// Only real thumbnails are returned (SIIGBF_THUMBNAILONLY): a file with no
/// thumbnail yields `None` rather than its file-type icon, so the UI can show
/// its "Preview unavailable" text. Any failure also yields `None`.
pub fn shell_thumbnail(path: &Path, size: u32) -> Option<image::RgbaImage> {
    let _com = ComGuard::new();
    let edge = i32::try_from(size).ok()?;
    let wide = HSTRING::from(path);
    // SAFETY: `wide` is a valid NUL-terminated path; the factory is a COM
    // interface released (Drop) before `_com` uninitializes COM.
    let factory: IShellItemImageFactory =
        unsafe { SHCreateItemFromParsingName(&wide, None) }.ok()?;
    let hbitmap = unsafe {
        factory.GetImage(
            SIZE { cx: edge, cy: edge },
            SIIGBF_RESIZETOFIT | SIIGBF_BIGGERSIZEOK | SIIGBF_THUMBNAILONLY,
        )
    }
    .ok()?;
    let bitmap = OwnedBitmap(hbitmap);
    read_bitmap(bitmap.0)
}

/// Deletes the HBITMAP on drop, so every return path frees it.
struct OwnedBitmap(HBITMAP);

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        // SAFETY: we own this bitmap (returned by GetImage) and it is not
        // selected into any DC.
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

/// Deletes the memory DC on drop.
struct OwnedDc(HDC);

impl Drop for OwnedDc {
    fn drop(&mut self) {
        // SAFETY: the DC came from CreateCompatibleDC and is owned by us.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

/// Copies an HBITMAP's pixels into an RGBA image (does not free the bitmap).
fn read_bitmap(hbitmap: HBITMAP) -> Option<image::RgbaImage> {
    let mut info = BITMAP::default();
    // SAFETY: `info` is a properly sized, writable BITMAP.
    let got = unsafe {
        GetObjectW(
            hbitmap.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut _),
        )
    };
    if got == 0 {
        return None;
    }
    let width = info.bmWidth;
    let height = info.bmHeight.checked_abs()?;
    if width <= 0 || height <= 0 {
        return None;
    }

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // negative = top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];

    // SAFETY: plain FFI; the DC is freed by OwnedDc.
    let dc = OwnedDc(unsafe { CreateCompatibleDC(None) });
    if dc.0.is_invalid() {
        return None;
    }
    // SAFETY: `pixels` holds exactly width*height 32-bpp pixels, matching the
    // header in `bmi`; the bitmap is not selected into any DC.
    let lines = unsafe {
        GetDIBits(
            dc.0,
            hbitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    if lines != height {
        return None;
    }

    bgra_to_rgba(&mut pixels);
    image::RgbaImage::from_raw(width as u32, height as u32, pixels)
}

/// Converts premultiplied BGRA (as the shell returns it) into straight RGBA.
/// Bitmaps whose alpha channel is entirely zero carry no alpha information and
/// are treated as opaque.
fn bgra_to_rgba(pixels: &mut [u8]) {
    let has_alpha = pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
    for p in pixels.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
        if !has_alpha {
            p[3] = 255;
        } else if p[3] != 0 && p[3] != 255 {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_thumbnail_of_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.png");
        image::RgbaImage::from_pixel(200, 100, image::Rgba([10, 200, 30, 255]))
            .save(&path)
            .unwrap();
        let thumb = shell_thumbnail(&path, 256).expect("shell thumbnail");
        assert!(thumb.width() > 0 && thumb.width() <= 256, "width {}", thumb.width());
        assert!(thumb.height() > 0);
        // Channel order and alpha survive the BGRA -> RGBA conversion.
        let p = thumb.get_pixel(thumb.width() / 2, thumb.height() / 2).0;
        let close = |a: u8, b: u8| a.abs_diff(b) <= 8;
        assert!(
            close(p[0], 10) && close(p[1], 200) && close(p[2], 30) && p[3] == 255,
            "center pixel {p:?}"
        );
    }

    #[test]
    fn shell_thumbnail_of_file_without_thumbnail_is_none() {
        // Plain text has no thumbnail handler; we must not get its icon back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        assert!(shell_thumbnail(&path, 256).is_none());
    }

    #[test]
    fn bgra_conversion_swaps_and_unpremultiplies() {
        // Opaque, half-transparent premultiplied, and fully transparent pixels.
        let mut px = vec![30, 20, 10, 255, 50, 25, 0, 128, 0, 0, 0, 0];
        bgra_to_rgba(&mut px);
        assert_eq!(px, vec![10, 20, 30, 255, 0, 50, 100, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn bgra_without_alpha_is_opaque() {
        let mut px = vec![1, 2, 3, 0, 4, 5, 6, 0];
        bgra_to_rgba(&mut px);
        assert_eq!(px, vec![3, 2, 1, 255, 6, 5, 4, 255]);
    }
}
