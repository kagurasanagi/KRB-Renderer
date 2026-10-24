//! 動画（MP4・H.264）の書き出し：Windows に入っている Media Foundation の H.264 エンコーダーを使う（FFmpeg などを
//! 入れなくてよい）。書き込みは裏のスレッドで行い、フレーム（RGBA8、上から下）を順に渡す。
//! 色は BT.709（リミテッドレンジ）の NV12 にして渡す（どの H.264 エンコーダーも受け付ける形）

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

/// 動画の書き出しの指定
#[derive(Debug, Clone)]
pub struct VideoFormat {
    pub path: PathBuf,
    /// 偶数（NV12 の決まり）
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// ビットレート（bit/s）
    pub bitrate: u32,
}

/// 書き込み中の動画。push でフレームを渡し、finish で閉じる
pub struct VideoWriter {
    tx: Option<mpsc::SyncSender<Vec<u8>>>,
    thread: Option<JoinHandle<Result<u64, String>>>,
    width: u32,
    height: u32,
}

impl VideoWriter {
    /// ファイルを作って書き始める（エンコーダーを用意できなければ Err）
    pub fn start(f: &VideoFormat) -> Result<Self, String> {
        if f.width < 2 || f.height < 2 || f.width % 2 != 0 || f.height % 2 != 0 {
            return Err(format!("the size of a video must be even: {}x{}", f.width, f.height));
        }
        if let Some(dir) = f.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        // 前のファイルがあると、エンコーダーは上書きしないことがある
        let _ = std::fs::remove_file(&f.path);
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(3);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let fmt = f.clone();
        let thread = std::thread::spawn(move || imp::run(&fmt, rx, ready_tx));
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx: Some(tx), thread: Some(thread), width: f.width, height: f.height }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(thread.join().ok().and_then(|r| r.err()).unwrap_or_else(|| "the video encoder stopped".into())),
        }
    }

    /// 1 フレームを渡す（RGBA8、幅 × 高さ × 4 バイト、上の行から）。前のフレームの書き込みを少し待つことがある
    pub fn push(&mut self, rgba: Vec<u8>) -> Result<(), String> {
        if rgba.len() != (self.width * self.height * 4) as usize {
            return Err(format!("a frame of {} bytes for {}x{}", rgba.len(), self.width, self.height));
        }
        let tx = self.tx.as_ref().ok_or("the video is closed")?;
        if tx.send(rgba).is_err() {
            // 書き込みのスレッドが止まった：理由を返す
            self.tx = None;
            return Err(self.thread.take().and_then(|t| t.join().ok()).and_then(|r| r.err()).unwrap_or_else(|| "the video encoder stopped".into()));
        }
        Ok(())
    }

    /// 書き終えて閉じる（書いたバイト数）。裏のスレッドで書き終えるのを待つ
    pub fn finish(mut self) -> Result<u64, String> {
        self.tx = None;
        match self.thread.take() {
            Some(t) => t.join().map_err(|_| "the video encoder crashed".to_string())?,
            None => Err("the video encoder stopped".into()),
        }
    }
}

impl Drop for VideoWriter {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 画質の目安から、ビットレート（bit/s）：1 画素・1 フレームあたりのビット数 × 画素数 × フレームレート
pub fn bitrate_for(width: u32, height: u32, fps: u32, bits_per_pixel: f64) -> u32 {
    ((width as f64 * height as f64 * fps as f64 * bits_per_pixel).round() as u64).clamp(200_000, 100_000_000) as u32
}

/// RGBA8（sRGB）を NV12（BT.709、16〜235）にする。幅・高さは偶数
pub fn rgba_to_nv12(rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let (y_plane, uv_plane) = out.split_at_mut(w * h);
    // BT.709 のリミテッドレンジ（8 bit）
    let ycoef = |r: f32, g: f32, b: f32| 16.0 + 219.0 * (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0;
    for y in 0..h {
        let row = &rgba[y * w * 4..(y + 1) * w * 4];
        let yr = &mut y_plane[y * w..(y + 1) * w];
        for x in 0..w {
            let p = &row[x * 4..x * 4 + 4];
            yr[x] = ycoef(p[0] as f32, p[1] as f32, p[2] as f32).round().clamp(0.0, 255.0) as u8;
        }
    }
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let mut s = [0f32; 3];
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let p = &rgba[((2 * y + dy) * w + 2 * x + dx) * 4..][..3];
                for c in 0..3 {
                    s[c] += p[c] as f32 / 4.0;
                }
            }
            let (r, g, b) = (s[0] / 255.0, s[1] / 255.0, s[2] / 255.0);
            let yl = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let cb = (b - yl) / 1.8556;
            let cr = (r - yl) / 1.5748;
            uv_plane[y * w + 2 * x] = (128.0 + 224.0 * cb).round().clamp(0.0, 255.0) as u8;
            uv_plane[y * w + 2 * x + 1] = (128.0 + 224.0 * cr).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// 動画を読んで、各フレームを RGBA8（上から下）にする（書き出した動画の確かめ用。Windows の Media Foundation で読む）
#[cfg(windows)]
pub fn decode_frames(path: &Path, max: usize) -> Result<Vec<(u32, u32, Vec<u8>)>, String> {
    imp::decode(path, max)
}

/// 書いたファイルの大きさ
fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::core::{HSTRING, GUID};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    /// MF_VERSION（MF_SDK_VERSION 2、MF_API_VERSION 0x70）
    const MF_VERSION: u32 = 0x0002_0070;

    fn e(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
        move |err| format!("{what}: {err}")
    }

    fn pack(a: u32, b: u32) -> u64 {
        ((a as u64) << 32) | b as u64
    }

    fn video_type(subtype: &GUID, f: &VideoFormat) -> Result<IMFMediaType, String> {
        unsafe {
            let t = MFCreateMediaType().map_err(e("media type"))?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("major type"))?;
            t.SetGUID(&MF_MT_SUBTYPE, subtype).map_err(e("subtype"))?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack(f.width, f.height)).map_err(e("frame size"))?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack(f.fps, 1)).map_err(e("frame rate"))?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).map_err(e("pixel aspect"))?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e("interlace"))?;
            t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32).map_err(e("matrix"))?;
            t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32).map_err(e("range"))?;
            Ok(t)
        }
    }

    pub(super) fn run(f: &VideoFormat, rx: mpsc::Receiver<Vec<u8>>, ready: mpsc::Sender<Result<(), String>>) -> Result<u64, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let r = (|| -> Result<u64, String> {
            unsafe {
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(e("Media Foundation (on Windows N editions, install the Media Feature Pack)"))?;
                let r = encode(f, &rx, &ready);
                let _ = MFShutdown();
                r
            }
        })();
        if let Err(msg) = &r {
            let _ = ready.send(Err(msg.clone()));
        }
        unsafe { CoUninitialize() };
        r
    }

    pub(super) fn decode(path: &Path, max: usize) -> Result<Vec<(u32, u32, Vec<u8>)>, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(e("Media Foundation"))?;
            let r = (|| -> Result<Vec<(u32, u32, Vec<u8>)>, String> {
                let mut attrs: Option<IMFAttributes> = None;
                MFCreateAttributes(&mut attrs, 1).map_err(e("attributes"))?;
                let attrs = attrs.ok_or("attributes")?;
                attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1).map_err(e("video processing"))?;
                let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs).map_err(e("cannot open the video"))?;
                let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
                let t = MFCreateMediaType().map_err(e("media type"))?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("major type"))?;
                t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(e("subtype"))?;
                reader.SetCurrentMediaType(stream, None, &t).map_err(e("RGB32 output"))?;
                let cur = reader.GetCurrentMediaType(stream).map_err(e("current type"))?;
                let size = cur.GetUINT64(&MF_MT_FRAME_SIZE).map_err(e("frame size"))?;
                let (w, h) = ((size >> 32) as u32, (size & 0xffff_ffff) as u32);
                let mut frames = Vec::new();
                while frames.len() < max {
                    let mut flags = 0u32;
                    let mut sample: Option<IMFSample> = None;
                    reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample)).map_err(e("read a frame"))?;
                    if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                        break;
                    }
                    let Some(sample) = sample else { continue };
                    let buf = sample.ConvertToContiguousBuffer().map_err(e("buffer"))?;
                    let mut ptr: *mut u8 = std::ptr::null_mut();
                    let mut len = 0u32;
                    buf.Lock(&mut ptr, None, Some(&mut len)).map_err(e("lock"))?;
                    let data = std::slice::from_raw_parts(ptr, len as usize);
                    // RGB32 は BGRX。行は上から（Media Foundation の既定の向き）
                    let mut rgba = vec![0u8; (w * h * 4) as usize];
                    for i in 0..(w * h) as usize {
                        if i * 4 + 3 < data.len() {
                            rgba[i * 4..i * 4 + 4].copy_from_slice(&[data[i * 4 + 2], data[i * 4 + 1], data[i * 4], 255]);
                        }
                    }
                    let _ = buf.Unlock();
                    frames.push((w, h, rgba));
                }
                Ok(frames)
            })();
            let _ = MFShutdown();
            r
        }
    }

    unsafe fn encode(f: &VideoFormat, rx: &mpsc::Receiver<Vec<u8>>, ready: &mpsc::Sender<Result<(), String>>) -> Result<u64, String> {
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 2).map_err(e("attributes"))?;
            let attrs = attrs.ok_or("attributes")?;
            // GPU のエンコーダーがあれば使う。書き込みの速さで呼び出し側を待たせない（フレームは自分で順に渡す）
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).map_err(e("hardware"))?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).map_err(e("throttling"))?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(f.path.as_os_str()), None, &attrs).map_err(e("cannot create the video file"))?;
            let out = video_type(&MFVideoFormat_H264, f)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, f.bitrate).map_err(e("bitrate"))?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32).map_err(e("profile"))?;
            let stream = writer.AddStream(&out).map_err(e("H.264 stream"))?;
            let input = video_type(&MFVideoFormat_NV12, f)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, f.width).map_err(e("stride"))?;
            writer.SetInputMediaType(stream, &input, None).map_err(e("the H.264 encoder does not accept the frames"))?;
            writer.BeginWriting().map_err(e("begin writing"))?;
            let _ = ready.send(Ok(()));
            let (w, h) = (f.width as usize, f.height as usize);
            let size = w * h * 3 / 2;
            // 時刻は 100 ns 単位
            let duration = 10_000_000i64 / f.fps as i64;
            let mut nv12 = vec![0u8; size];
            let mut index = 0i64;
            for rgba in rx.iter() {
                rgba_to_nv12(&rgba, w, h, &mut nv12);
                let buf = MFCreateMemoryBuffer(size as u32).map_err(e("buffer"))?;
                let mut ptr: *mut u8 = std::ptr::null_mut();
                buf.Lock(&mut ptr, None, None).map_err(e("lock"))?;
                std::ptr::copy_nonoverlapping(nv12.as_ptr(), ptr, size);
                buf.Unlock().map_err(e("unlock"))?;
                buf.SetCurrentLength(size as u32).map_err(e("length"))?;
                let sample = MFCreateSample().map_err(e("sample"))?;
                sample.AddBuffer(&buf).map_err(e("add buffer"))?;
                sample.SetSampleTime(index * duration).map_err(e("time"))?;
                sample.SetSampleDuration(duration).map_err(e("duration"))?;
                writer.WriteSample(stream, &sample).map_err(e("write a frame"))?;
                index += 1;
            }
            writer.Finalize().map_err(e("finish the video file"))?;
            Ok(file_size(&f.path))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub(super) fn run(_f: &VideoFormat, _rx: mpsc::Receiver<Vec<u8>>, ready: mpsc::Sender<Result<(), String>>) -> Result<u64, String> {
        let msg = "video export needs Windows (Media Foundation)".to_string();
        let _ = ready.send(Err(msg.clone()));
        Err(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 書き出した動画のフレームを PNG にして見る（KRBR_VIDEO_DUMP=動画のパス。cargo test -- --ignored dump_video_frames）
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn dump_video_frames() {
        let Some(p) = std::env::var_os("KRBR_VIDEO_DUMP") else { return };
        let p = PathBuf::from(p);
        let frames = decode_frames(&p, 1000).expect("decode");
        println!("{} frames", frames.len());
        for (i, (w, h, rgba)) in frames.iter().enumerate() {
            let out = p.with_file_name(format!("{}_{i:03}.png", p.file_stem().unwrap().to_string_lossy()));
            krbr_render::image_io::save_png_rgba8(&out, *w, *h, rgba).unwrap();
        }
    }

    #[test]
    fn nv12_of_gray_and_colors() {
        let (w, h) = (4, 2);
        let mut rgba = vec![0u8; w * h * 4];
        for p in rgba.chunks_exact_mut(4) {
            p.copy_from_slice(&[255, 255, 255, 255]);
        }
        let mut out = vec![0u8; w * h * 3 / 2];
        rgba_to_nv12(&rgba, w, h, &mut out);
        assert!(out[..w * h].iter().all(|&y| y == 235));
        assert!(out[w * h..].iter().all(|&c| c == 128));
        assert_eq!(bitrate_for(1920, 1080, 30, 0.1), 6_220_800);
    }

    /// 小さな動画を書いてみる（Media Foundation の H.264 エンコーダーが使えること）
    #[cfg(windows)]
    #[test]
    fn writes_a_small_mp4() {
        let path = std::env::temp_dir().join(format!("krbr_video_test_{}.mp4", std::process::id()));
        let f = VideoFormat { path: path.clone(), width: 160, height: 90, fps: 30, bitrate: 500_000 };
        let mut v = VideoWriter::start(&f).expect("start");
        for i in 0..30u32 {
            let mut rgba = vec![0u8; 160 * 90 * 4];
            for (j, p) in rgba.chunks_exact_mut(4).enumerate() {
                let x = (j % 160) as u32;
                p.copy_from_slice(&[((x + i * 4) % 256) as u8, 80, 160, 255]);
            }
            v.push(rgba).expect("push");
        }
        let bytes = v.finish().expect("finish");
        assert!(bytes > 1000, "{bytes} bytes");
        let head = std::fs::read(&path).unwrap();
        assert_eq!(&head[4..8], b"ftyp");
        // 読み戻すと、同じ数のフレームで、色もほぼ同じ（緑 80・青 160 の帯）
        let frames = decode_frames(&path, 100).expect("decode");
        assert_eq!(frames.len(), 30);
        let (w, h, rgba) = &frames[0];
        assert_eq!((*w, *h), (160, 90));
        let px = &rgba[((45 * 160 + 80) * 4) as usize..][..3];
        assert!((px[1] as i32 - 80).abs() < 12 && (px[2] as i32 - 160).abs() < 12, "{px:?}");
        let _ = std::fs::remove_file(&path);
    }
}
