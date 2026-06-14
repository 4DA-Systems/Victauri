//! Native window capture. `capture_window_raw` returns straight RGBA bytes plus
//! dimensions; `capture_window` wraps that into a PNG. The raw form feeds the
//! `filmstrip` compositor used by the animation `scrub` tool. Raw capture is
//! available on Windows, macOS, and Linux/X11. Pure Wayland capture fails
//! safely because its available fallback is a full-desktop image, not the
//! requested application window.

/// Capture a window as a PNG (RGBA, 8-bit).
#[cfg(windows)]
#[allow(dead_code)]
pub async fn capture_window(hwnd: isize) -> anyhow::Result<Vec<u8>> {
    let (rgba, w, h) = capture_window_raw(hwnd).await?;
    tokio::task::spawn_blocking(move || encode_png(w, h, &rgba)).await?
}

/// Capture a window as straight RGBA bytes + (width, height).
#[cfg(windows)]
#[allow(dead_code, unsafe_code)]
pub async fn capture_window_raw(hwnd: isize) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HBITMAP, HDC, HGDIOBJ, ReleaseDC,
        SRCCOPY, SelectObject,
    };
    use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PW_CLIENTONLY, PrintWindow};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetClientRect, GetWindowLongPtrW, PW_RENDERFULLCONTENT, WS_EX_LAYERED,
        WS_EX_NOREDIRECTIONBITMAP,
    };

    /// RAII guard that releases GDI handles on drop, preventing leaks when
    /// early returns (`?`) occur after handle acquisition.
    struct GdiGuard {
        hwnd: HWND,
        hdc_screen: HDC,
        hdc_mem: HDC,
        hbmp: HBITMAP,
        old: HGDIOBJ,
    }

    impl Drop for GdiGuard {
        fn drop(&mut self) {
            // SAFETY: All handles were acquired from valid Win32 GDI calls in
            // the enclosing `capture_window` function. They must be released in
            // reverse acquisition order: restore the original bitmap, delete the
            // compatible bitmap, delete the memory DC, and release the screen DC.
            unsafe {
                SelectObject(self.hdc_mem, self.old);
                let _ = DeleteObject(self.hbmp.into());
                let _ = DeleteDC(self.hdc_mem);
                ReleaseDC(Some(self.hwnd), self.hdc_screen);
            }
        }
    }

    tokio::task::spawn_blocking(move || {
        // SAFETY: All Win32 GDI calls operate on handles obtained from the
        // provided `hwnd` window handle. The `GdiGuard` ensures every acquired
        // handle is released even if an early `?` return occurs (e.g. BitBlt
        // failure). The pixel buffer is correctly sized for the window
        // dimensions before being passed to `GetDIBits`.
        unsafe {
            let hwnd = HWND(hwnd as *mut _);
            let mut rect = std::mem::zeroed();
            GetClientRect(hwnd, &mut rect)?;

            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            if width <= 0 || height <= 0 {
                anyhow::bail!("window has zero area ({width}x{height})");
            }

            let hdc_screen = GetDC(Some(hwnd));
            let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
            let hbmp = CreateCompatibleBitmap(hdc_screen, width, height);
            let old = SelectObject(hdc_mem, hbmp.into());

            let _guard = GdiGuard {
                hwnd,
                hdc_screen,
                hdc_mem,
                hbmp,
                old,
            };

            let flags = PRINT_WINDOW_FLAGS(PW_CLIENTONLY.0 | PW_RENDERFULLCONTENT);
            let captured = PrintWindow(hwnd, hdc_mem, flags);
            if !captured.as_bool() {
                BitBlt(
                    hdc_mem,
                    0,
                    0,
                    width,
                    height,
                    Some(hdc_screen),
                    0,
                    0,
                    SRCCOPY,
                )?;
            }

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..std::mem::zeroed()
                },
                ..std::mem::zeroed()
            };

            let row_bytes = (width as usize)
                .checked_mul(4)
                .ok_or_else(|| anyhow::anyhow!("screenshot buffer overflow: width={width}"))?;
            let total = row_bytes
                .checked_mul(height as usize)
                .ok_or_else(|| anyhow::anyhow!("screenshot buffer overflow: {width}x{height}"))?;
            let mut pixels = vec![0u8; total];
            let rows = GetDIBits(
                hdc_mem,
                hbmp,
                0,
                height as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut bmi,
                DIB_RGB_COLORS,
            );

            if rows == 0 {
                anyhow::bail!("GetDIBits failed to read pixel data from window");
            }

            // BGRA → RGBA
            for chunk in pixels.chunks_exact_mut(4) {
                chunk.swap(0, 2);
            }

            // Fail loudly on the silent-blank failure mode instead of returning a
            // white/empty PNG that looks like a successful capture. Transparent or
            // GPU-composited windows (e.g. a Tauri `transparent: true` notification
            // window) have no GDI redirection surface for PrintWindow/BitBlt to copy,
            // so the capture comes back uniform. This is the exact wall the
            // `animation scrub` filmstrip path hits — surface it as an actionable
            // error pointing at the OS-composite workaround.
            if let Some(reason) = blank_frame_reason(&pixels) {
                let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                let composited = ex_style & (WS_EX_LAYERED.0 | WS_EX_NOREDIRECTIONBITMAP.0) != 0;
                let hint = if composited {
                    "the window is layered/composited (WS_EX_LAYERED or \
                     WS_EX_NOREDIRECTIONBITMAP is set — typical of transparent Tauri \
                     windows)"
                } else {
                    "this is typical of transparent or GPU-composited webview content"
                };
                anyhow::bail!(
                    "window capture returned a {reason} {width}x{height} frame; {hint}. \
                     GDI capture (PrintWindow/BitBlt) cannot see transparent/composited \
                     windows — use an OS desktop-composite grab (e.g. ffmpeg gdigrab) \
                     for this window."
                );
            }

            Ok((pixels, width as u32, height as u32))
        }
    })
    .await?
}

/// Detects the silent-blank capture failure: GDI capture of a transparent or
/// GPU-composited window (no DWM redirection surface) copies an empty buffer and
/// returns a uniform white or empty frame with no error. Returns
/// `Some(description)` for such a frame so the caller can fail loudly instead of
/// handing back a blank image. Only RGB is compared — the alpha byte from
/// `BI_RGB` 32-bpp capture is undefined.
#[cfg(windows)]
fn blank_frame_reason(pixels: &[u8]) -> Option<&'static str> {
    let first = pixels.chunks_exact(4).next()?;
    if pixels.chunks_exact(4).any(|p| p[..3] != first[..3]) {
        return None; // colour variation => real content captured
    }
    match (first[0], first[1], first[2]) {
        (0xFF, 0xFF, 0xFF) => Some("blank white"),
        (0x00, 0x00, 0x00) => Some("blank (empty)"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub async fn capture_window(window_id: isize) -> anyhow::Result<Vec<u8>> {
    let (rgba, w, h) = capture_window_raw(window_id).await?;
    tokio::task::spawn_blocking(move || encode_png(w, h, &rgba)).await?
}

#[cfg(target_os = "macos")]
#[allow(dead_code, unsafe_code)]
pub async fn capture_window_raw(window_id: isize) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    tokio::task::spawn_blocking(move || unsafe {
        // CoreGraphics FFI types and functions
        #[allow(non_camel_case_types)]
        type CGWindowID = u32;
        #[allow(non_camel_case_types)]
        type CGFloat = f64;
        #[allow(non_camel_case_types)]
        type CGWindowListOption = u32;
        #[allow(non_camel_case_types)]
        type CGWindowImageOption = u32;

        type CFTypeRef = *const std::ffi::c_void;
        type CGImageRef = *const std::ffi::c_void;
        type CGColorSpaceRef = *const std::ffi::c_void;
        type CGContextRef = *const std::ffi::c_void;
        type CGDataProviderRef = *const std::ffi::c_void;
        type CFDataRef = *const std::ffi::c_void;

        #[repr(C)]
        #[derive(Clone, Copy)]
        struct CGRect {
            origin: CGPoint,
            size: CGSize,
        }

        #[repr(C)]
        #[derive(Clone, Copy)]
        struct CGPoint {
            x: CGFloat,
            y: CGFloat,
        }

        #[repr(C)]
        #[derive(Clone, Copy)]
        struct CGSize {
            width: CGFloat,
            height: CGFloat,
        }

        // CGWindowListOption constants
        const K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW: CGWindowListOption = 1 << 3;

        // CGWindowImageOption constants
        #[allow(dead_code)]
        const K_CG_WINDOW_IMAGE_DEFAULT: CGWindowImageOption = 0;
        const K_CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING: CGWindowImageOption = 1 << 0;
        const K_CG_WINDOW_IMAGE_SHOULD_BE_OPAQUE: CGWindowImageOption = 1 << 1;

        // CGBitmapInfo constants
        const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
        const K_CG_BITMAP_BYTE_ORDER_32_BIG: u32 = 4 << 12;

        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGWindowListCreateImage(
                screenBounds: CGRect,
                listOption: CGWindowListOption,
                windowID: CGWindowID,
                imageOption: CGWindowImageOption,
            ) -> CGImageRef;
            fn CGImageGetWidth(image: CGImageRef) -> usize;
            fn CGImageGetHeight(image: CGImageRef) -> usize;
            fn CGImageGetBitsPerComponent(image: CGImageRef) -> usize;
            fn CGImageGetBitsPerPixel(image: CGImageRef) -> usize;
            fn CGImageGetBytesPerRow(image: CGImageRef) -> usize;
            fn CGImageGetDataProvider(image: CGImageRef) -> CGDataProviderRef;
            fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
            fn CGBitmapContextCreate(
                data: *mut u8,
                width: usize,
                height: usize,
                bitsPerComponent: usize,
                bytesPerRow: usize,
                space: CGColorSpaceRef,
                bitmapInfo: u32,
            ) -> CGContextRef;
            fn CGContextDrawImage(c: CGContextRef, rect: CGRect, image: CGImageRef);
            fn CGContextRelease(c: CGContextRef);
            fn CGColorSpaceRelease(space: CGColorSpaceRef);
            fn CGDataProviderCopyData(provider: CGDataProviderRef) -> CFDataRef;
            fn CGImageGetAlphaInfo(image: CGImageRef) -> u32;
        }

        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFDataGetBytePtr(theData: CFDataRef) -> *const u8;
            fn CFDataGetLength(theData: CFDataRef) -> isize;
            fn CFRelease(cf: CFTypeRef);
        }

        // Null rect means "capture the minimum bounding rect for the window"
        let cg_rect_null = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };

        let cg_window_id: CGWindowID = window_id as CGWindowID;

        // Capture the window image
        let image = CGWindowListCreateImage(
            cg_rect_null,
            K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
            cg_window_id,
            K_CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING | K_CG_WINDOW_IMAGE_SHOULD_BE_OPAQUE,
        );

        if image.is_null() {
            anyhow::bail!(
                "CGWindowListCreateImage returned null for window ID {cg_window_id}. \
                 The window may not exist or screen recording permission may be required."
            );
        }

        let width = CGImageGetWidth(image);
        let height = CGImageGetHeight(image);

        if width == 0 || height == 0 {
            CFRelease(image);
            anyhow::bail!("captured image has zero area ({width}x{height})");
        }

        // Draw the CGImage into a known-format RGBA bitmap context.
        // This normalizes any source pixel format (BGRA, premultiplied, etc.)
        // into straight RGBA that our PNG encoder expects.
        let bytes_per_row = width
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!("screenshot buffer overflow: width={width}"))?;
        let total = bytes_per_row
            .checked_mul(height)
            .ok_or_else(|| anyhow::anyhow!("screenshot buffer overflow: {width}x{height}"))?;
        let mut rgba_pixels = vec![0u8; total];

        let color_space = CGColorSpaceCreateDeviceRGB();
        if color_space.is_null() {
            CFRelease(image);
            anyhow::bail!("CGColorSpaceCreateDeviceRGB returned null");
        }

        let bitmap_info = K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST | K_CG_BITMAP_BYTE_ORDER_32_BIG;

        let context = CGBitmapContextCreate(
            rgba_pixels.as_mut_ptr(),
            width,
            height,
            8, // bits per component
            bytes_per_row,
            color_space,
            bitmap_info,
        );

        if context.is_null() {
            CGColorSpaceRelease(color_space);
            CFRelease(image);
            anyhow::bail!("CGBitmapContextCreate returned null");
        }

        let draw_rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: width as CGFloat,
                height: height as CGFloat,
            },
        };

        CGContextDrawImage(context, draw_rect, image);
        CGContextRelease(context);
        CGColorSpaceRelease(color_space);
        CFRelease(image);

        // Un-premultiply alpha.
        // CoreGraphics gives us premultiplied RGBA. The PNG spec requires
        // straight (non-premultiplied) alpha, so we reverse the operation.
        for chunk in rgba_pixels.chunks_exact_mut(4) {
            let a = u16::from(chunk[3]);
            if a > 0 && a < 255 {
                chunk[0] = ((u16::from(chunk[0]) * 255 + a / 2) / a).min(255) as u8;
                chunk[1] = ((u16::from(chunk[1]) * 255 + a / 2) / a).min(255) as u8;
                chunk[2] = ((u16::from(chunk[2]) * 255 + a / 2) / a).min(255) as u8;
            }
        }

        Ok((rgba_pixels, width as u32, height as u32))
    })
    .await?
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub async fn capture_window(window_id: isize) -> anyhow::Result<Vec<u8>> {
    // Try X11 first (works on X11 and XWayland)
    match capture_window_x11_raw(window_id).await {
        Ok((rgba, w, h)) => tokio::task::spawn_blocking(move || encode_png(w, h, &rgba)).await?,
        Err(x11_err) => {
            anyhow::bail!(
                "window screenshot requires X11/XWayland on Linux ({x11_err}); \
                 pure Wayland does not expose a safe per-window capture path, and \
                 Victauri refuses to capture the full desktop"
            );
        }
    }
}

/// Raw RGBA capture (X11 only).
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub async fn capture_window_raw(window_id: isize) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    capture_window_x11_raw(window_id).await.map_err(|e| {
        anyhow::anyhow!(
            "raw window capture requires X11 ({e}); not available on pure Wayland — \
             use animation scrub without capture to get the geometry curve only"
        )
    })
}

#[cfg(target_os = "linux")]
async fn capture_window_x11_raw(window_id: isize) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};

    tokio::task::spawn_blocking(move || {
        let (conn, _screen_num) =
            x11rb::connect(None).map_err(|e| anyhow::anyhow!("X11 connect failed: {e}"))?;

        let window = window_id as u32;
        let geom = conn
            .get_geometry(window)
            .map_err(|e| anyhow::anyhow!("get_geometry failed: {e}"))?
            .reply()
            .map_err(|e| anyhow::anyhow!("get_geometry reply failed: {e}"))?;

        let width = u32::from(geom.width);
        let height = u32::from(geom.height);
        if width == 0 || height == 0 {
            anyhow::bail!("window has zero area ({width}x{height})");
        }

        let image = conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                window,
                0,
                0,
                geom.width,
                geom.height,
                !0,
            )
            .map_err(|e| anyhow::anyhow!("get_image failed: {e}"))?
            .reply()
            .map_err(|e| anyhow::anyhow!("get_image reply failed: {e}"))?;

        let data = image.data;
        let depth = image.depth;

        let rgba = if depth == 32 || depth == 24 {
            // X11 ZPixmap with depth 24/32 is typically BGRA or BGRx
            let mut pixels = Vec::with_capacity(data.len());
            for chunk in data.chunks_exact(4) {
                pixels.push(chunk[2]); // R
                pixels.push(chunk[1]); // G
                pixels.push(chunk[0]); // B
                pixels.push(if depth == 32 { chunk[3] } else { 255 }); // A
            }
            pixels
        } else {
            anyhow::bail!("unsupported X11 depth: {depth} (expected 24 or 32)");
        };

        Ok((rgba, width, height))
    })
    .await?
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
#[allow(dead_code)]
pub async fn capture_window(_window_id: isize) -> anyhow::Result<Vec<u8>> {
    anyhow::bail!("screenshot capture not yet implemented for this platform")
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
#[allow(dead_code)]
pub async fn capture_window_raw(_window_id: isize) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    anyhow::bail!("raw window capture not yet implemented for this platform")
}

const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
const PNG_BIT_DEPTH: u8 = 8;
const PNG_COLOR_TYPE_RGBA: u8 = 6;
const PNG_OVERHEAD_BYTES: usize = 45;
const PNG_FILTER_OVERHEAD_PER_ROW: usize = 6;
const IHDR_DATA_LEN: usize = 13;
const CRC32_INIT: u32 = 0xFFFF_FFFF;
const CRC32_POLYNOMIAL: u32 = 0xEDB8_8320;
const ADLER32_MOD: u32 = 65521;

#[allow(dead_code)]
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;

    let mut out = Vec::with_capacity(
        PNG_OVERHEAD_BYTES + rgba.len() + (height as usize) * PNG_FILTER_OVERHEAD_PER_ROW,
    );

    out.write_all(&PNG_SIGNATURE)?;

    let mut ihdr = Vec::with_capacity(IHDR_DATA_LEN);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(PNG_BIT_DEPTH);
    ihdr.push(PNG_COLOR_TYPE_RGBA);
    ihdr.push(0); // compression: deflate
    ihdr.push(0); // filter: adaptive
    ihdr.push(0); // interlace: none
    write_png_chunk(&mut out, b"IHDR", &ihdr)?;

    // IDAT — raw pixel data with filter byte per row, deflate-compressed
    let row_len = (width as usize) * 4;
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for row in rgba.chunks_exact(row_len) {
        raw.push(0); // no filter
        raw.extend_from_slice(row);
    }

    let compressed = deflate_compress(&raw)?;
    write_png_chunk(&mut out, b"IDAT", &compressed)?;

    // IEND
    write_png_chunk(&mut out, b"IEND", &[])?;

    Ok(out)
}

#[allow(dead_code)]
fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;

    let len: u32 = data
        .len()
        .try_into()
        .map_err(|_| anyhow::anyhow!("PNG chunk too large: {} bytes", data.len()))?;
    out.write_all(&len.to_be_bytes())?;
    out.write_all(chunk_type)?;
    out.write_all(data)?;

    let mut crc_data = Vec::with_capacity(4 + data.len());
    crc_data.extend_from_slice(chunk_type);
    crc_data.extend_from_slice(data);
    let crc = png_crc32(&crc_data);
    out.write_all(&crc.to_be_bytes())?;

    Ok(())
}

#[allow(dead_code)]
fn png_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = CRC32_INIT;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ CRC32_POLYNOMIAL;
            } else {
                crc >>= 1;
            }
        }
    }
    crc ^ CRC32_INIT
}

#[allow(dead_code)]
fn deflate_compress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(data)
        .map_err(|e| anyhow::anyhow!("zlib write failed: {e}"))?;
    encoder
        .finish()
        .map_err(|e| anyhow::anyhow!("zlib finish failed: {e}"))
}

#[allow(dead_code)]
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + u32::from(byte)) % ADLER32_MOD;
        b = (b + a) % ADLER32_MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_signature_correct() {
        let rgba = vec![255, 0, 0, 255]; // 1x1 red pixel
        let png = encode_png(1, 1, &rgba).unwrap();
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn png_ihdr_chunk_present() {
        let rgba = vec![0u8; 4]; // 1x1 black pixel
        let png = encode_png(1, 1, &rgba).unwrap();
        // IHDR should be right after signature (8 bytes)
        // chunk: 4 bytes length + 4 bytes type
        assert_eq!(&png[12..16], b"IHDR");
    }

    #[test]
    fn png_iend_chunk_present() {
        let rgba = vec![0u8; 4];
        let png = encode_png(1, 1, &rgba).unwrap();
        // IEND should be at the end: 4 bytes length (0) + "IEND" + 4 bytes CRC
        let len = png.len();
        assert_eq!(&png[len - 8..len - 4], b"IEND");
    }

    #[test]
    fn png_2x2_produces_valid_output() {
        // 2x2 RGBA: red, green, blue, white
        let rgba = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];
        let png = encode_png(2, 2, &rgba).unwrap();
        // Should be a valid PNG (starts with signature, has IHDR, IDAT, IEND)
        assert!(png.len() > 50);
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn adler32_empty() {
        assert_eq!(adler32(&[]), 1);
    }

    #[test]
    fn adler32_known_value() {
        // adler32("Wikipedia") = 0x11E60398
        assert_eq!(adler32(b"Wikipedia"), 0x11E60398);
    }

    #[test]
    fn crc32_known_value() {
        // CRC32 of "IEND" = 0xAE426082
        assert_eq!(png_crc32(b"IEND"), 0xAE426082);
    }

    #[test]
    fn deflate_compress_roundtrip_structure() {
        use flate2::read::ZlibDecoder;
        use std::io::Read;

        let data = b"hello world";
        let compressed = deflate_compress(data).unwrap();
        // zlib header: CMF=0x78 (deflate, 32K window)
        assert_eq!(compressed[0], 0x78);
        let mut decoder = ZlibDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(&decompressed, data);
    }

    #[test]
    fn deflate_compress_large_data_compresses() {
        let data = vec![0u8; 100_000];
        let compressed = deflate_compress(&data).unwrap();
        // Uniform data should compress significantly
        assert!(
            compressed.len() < data.len() / 2,
            "expected significant compression, got {} -> {}",
            data.len(),
            compressed.len()
        );
    }

    #[test]
    fn encode_png_large_image() {
        // 100x100 image
        let rgba = vec![128u8; 100 * 100 * 4];
        let png = encode_png(100, 100, &rgba).unwrap();
        assert!(png.len() > 100);
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    // Blank-frame detection guards against the silent-white capture of a
    // transparent/composited window (the exact failure the 4DA notification
    // window hit). Windows-only because the detector is `#[cfg(windows)]`.
    #[cfg(windows)]
    #[test]
    fn uniform_white_reads_as_blank() {
        let px = vec![0xFFu8; 4 * 16];
        assert_eq!(blank_frame_reason(&px), Some("blank white"));
    }

    #[cfg(windows)]
    #[test]
    fn uniform_zero_reads_as_blank() {
        let px = vec![0x00u8; 4 * 16];
        assert_eq!(blank_frame_reason(&px), Some("blank (empty)"));
    }

    #[cfg(windows)]
    #[test]
    fn varied_content_is_not_blank() {
        let mut px = vec![0xFFu8; 4 * 16];
        px[20] = 0x10; // a single differing colour channel => real content
        assert_eq!(blank_frame_reason(&px), None);
    }

    #[cfg(windows)]
    #[test]
    fn uniform_grey_is_not_blank() {
        // A genuinely solid mid-grey window must not be flagged.
        let px = vec![0x80u8; 4 * 16];
        assert_eq!(blank_frame_reason(&px), None);
    }

    #[cfg(windows)]
    #[test]
    fn alpha_noise_ignored_for_white() {
        // RGB uniform white with varying (undefined) alpha still reads blank.
        let mut px = vec![0xFFu8; 4 * 4];
        px[3] = 0x00;
        px[7] = 0x12;
        assert_eq!(blank_frame_reason(&px), Some("blank white"));
    }
}
