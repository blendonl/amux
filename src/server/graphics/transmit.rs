use std::borrow::Cow;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use base64::alphabet;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine;
use miniz_oxide::deflate::compress_to_vec_zlib;
use miniz_oxide::inflate::stream::{inflate, InflateState};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use nix::fcntl::OFlag;

use super::command::Command;
use super::respond::{Code, Failure};
use super::store::Buffer;
use crate::protocol::ImageFormat;

pub const MAX_DATA_LEN: usize = 64 * 1024 * 1024;
const MAX_DIMENSION: u32 = 10_000;
const DEFLATE_LEVEL: u8 = 1;
const INFLATE_WINDOW: usize = 64 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const PNG_HEADER_LEN: usize = 24;
const TEMP_FILE_MARKER: &[u8] = b"tty-graphics-protocol";
const REFUSED_DIRS: [&str; 3] = ["/proc", "/sys", "/dev"];
const SHARED_MEMORY_DIR: &str = "/dev/shm";
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transmission {
    pub command: Command,
    pub data: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Waiting,
    Complete(Transmission),
    Failed(Command, Failure),
    Swallowed,
}

#[derive(Debug, Default)]
pub struct Transmissions {
    main: Option<Pending>,
    alt: Option<Pending>,
}

#[derive(Debug)]
struct Pending {
    command: Command,
    data: Option<Base64Stream>,
}

impl Transmissions {
    pub fn receive(&mut self, buffer: Buffer, mut command: Command) -> Step {
        let slot = match buffer {
            Buffer::Main => &mut self.main,
            Buffer::Alt => &mut self.alt,
        };
        let payload = mem::take(&mut command.payload);
        let mut pending = match slot.take() {
            Some(mut pending) if command.medium == b'd' => {
                if command.quiet != 0 {
                    pending.command.quiet = command.quiet;
                }
                pending.command.more = command.more;
                pending
            }
            _ if command.id != 0 && command.number != 0 => {
                let failure =
                    Failure::new(Code::Einval, "an image can't have both an id and a number");
                return Pending::failed(command, slot, failure);
            }
            _ => Pending {
                command,
                data: Some(Base64Stream::default()),
            },
        };
        let Some(data) = pending.data.as_mut() else {
            if pending.command.more {
                *slot = Some(pending);
            }
            return Step::Swallowed;
        };
        if let Err(failure) = data.push(&payload) {
            return Pending::failed(pending.command, slot, failure);
        }
        if pending.command.more {
            *slot = Some(pending);
            return Step::Waiting;
        }
        match pending.data.take().map(Base64Stream::finish) {
            Some(Ok(data)) => Step::Complete(Transmission {
                command: pending.command,
                data,
            }),
            Some(Err(failure)) => Step::Failed(pending.command, failure),
            None => Step::Swallowed,
        }
    }
}

impl Pending {
    fn failed(command: Command, slot: &mut Option<Self>, failure: Failure) -> Step {
        if command.more {
            *slot = Some(Self {
                command: command.clone(),
                data: None,
            });
        }
        Step::Failed(command, failure)
    }
}

#[derive(Debug, Default)]
struct Base64Stream {
    decoded: Vec<u8>,
    carry: Vec<u8>,
}

impl Base64Stream {
    fn push(&mut self, mut chunk: &[u8]) -> Result<(), Failure> {
        if !self.carry.is_empty() {
            let wanted = (4 - self.carry.len()).min(chunk.len());
            self.carry.extend_from_slice(&chunk[..wanted]);
            chunk = &chunk[wanted..];
            if self.carry.len() < 4 {
                return Ok(());
            }
            decode_base64(&self.carry, &mut self.decoded)?;
            self.carry.clear();
        }
        let whole = chunk.len() - chunk.len() % 4;
        decode_base64(&chunk[..whole], &mut self.decoded)?;
        self.carry.extend_from_slice(&chunk[whole..]);
        if self.decoded.len() > MAX_DATA_LEN {
            return Err(too_big());
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>, Failure> {
        decode_base64(&self.carry, &mut self.decoded)?;
        Ok(self.decoded)
    }
}

fn decode_base64(encoded: &[u8], decoded: &mut Vec<u8>) -> Result<(), Failure> {
    BASE64
        .decode_vec(encoded, decoded)
        .map_err(|_| Failure::new(Code::Einval, "the payload is not valid base64"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    pub compressed: bool,
    pub bytes: Vec<u8>,
    pub decoded_len: usize,
}

impl Image {
    pub fn packed(mut self) -> Self {
        if !self.compressed && self.format != ImageFormat::Png {
            self.bytes = compress_to_vec_zlib(&self.bytes, DEFLATE_LEVEL);
            self.compressed = true;
        }
        self
    }
}

pub fn load(command: &Command, data: Vec<u8>) -> Result<Image, Failure> {
    let bytes = match command.medium {
        b'd' => data,
        b'f' => read_file(&data, command, false)?,
        b't' => read_file(&data, command, true)?,
        b's' => read_shared_memory(&data, command)?,
        _ => return Err(Failure::new(Code::Einval, "unknown transmission medium")),
    };
    decode(command, bytes)
}

fn decode(command: &Command, mut bytes: Vec<u8>) -> Result<Image, Failure> {
    let compressed = match command.compression {
        None => false,
        Some(b'z') => true,
        Some(_) => return Err(Failure::new(Code::Einval, "unknown compression")),
    };
    let (format, bytes_per_pixel) = match command.format {
        24 => (ImageFormat::Rgb24, 3),
        32 => (ImageFormat::Rgba32, 4),
        100 => (ImageFormat::Png, 4),
        _ => return Err(Failure::new(Code::Einval, "unknown image format")),
    };
    let (width, height) = match format {
        ImageFormat::Png => png_size(&bytes, compressed)?,
        _ if command.width == 0 || command.height == 0 => {
            return Err(Failure::new(
                Code::Einval,
                "raw images need a width and a height",
            ))
        }
        _ => (command.width, command.height),
    };
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(Failure::new(
            Code::Efbig,
            format!("{width}x{height} is larger than {MAX_DIMENSION}x{MAX_DIMENSION}"),
        ));
    }
    let decoded_len = width as usize * height as usize * bytes_per_pixel;
    if format != ImageFormat::Png {
        let available = if compressed {
            inflate_up_to(&bytes, decoded_len, |_| {})?
        } else {
            bytes.len()
        };
        if available < decoded_len {
            return Err(Failure::new(
                Code::Enodata,
                format!("insufficient image data: {available} < {decoded_len}"),
            ));
        }
        if !compressed {
            bytes.truncate(decoded_len);
        }
    }
    Ok(Image {
        width,
        height,
        format,
        compressed,
        bytes,
        decoded_len,
    })
}

fn png_size(bytes: &[u8], compressed: bool) -> Result<(u32, u32), Failure> {
    let header: Cow<'_, [u8]> = if compressed {
        let mut header = Vec::with_capacity(PNG_HEADER_LEN);
        inflate_up_to(bytes, PNG_HEADER_LEN, |piece| {
            header.extend_from_slice(piece);
        })?;
        Cow::Owned(header)
    } else {
        Cow::Borrowed(bytes)
    };
    let bad = || Failure::new(Code::Ebadpng, "not a PNG image");
    let header = header.get(..PNG_HEADER_LEN).ok_or_else(bad)?;
    if !header.starts_with(PNG_SIGNATURE) || &header[12..16] != b"IHDR" {
        return Err(bad());
    }
    let number = |at: usize| {
        u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    let (width, height) = (number(16), number(20));
    if width == 0 || height == 0 {
        return Err(bad());
    }
    Ok((width, height))
}

fn inflate_up_to(
    compressed: &[u8],
    limit: usize,
    mut sink: impl FnMut(&[u8]),
) -> Result<usize, Failure> {
    let mut state = InflateState::new_boxed(DataFormat::Zlib);
    let mut window = vec![0; INFLATE_WINDOW.min(limit.max(1))];
    let mut input = compressed;
    let mut total = 0;
    while total < limit {
        let result = inflate(&mut state, input, &mut window, MZFlush::None);
        input = &input[result.bytes_consumed..];
        let written = result.bytes_written.min(limit - total);
        sink(&window[..written]);
        total += written;
        match result.status {
            Ok(MZStatus::StreamEnd) | Err(MZError::Buf) => break,
            Ok(_) if result.bytes_consumed == 0 && result.bytes_written == 0 => break,
            Ok(_) => {}
            Err(_) => return Err(Failure::new(Code::Einval, "the zlib data is corrupt")),
        }
    }
    Ok(total)
}

fn read_file(path: &[u8], command: &Command, temporary: bool) -> Result<Vec<u8>, Failure> {
    let path = Path::new(OsStr::from_bytes(path));
    if !path.is_absolute() {
        return Err(Failure::new(Code::Einval, "the file path must be absolute"));
    }
    let canonical = fs::canonicalize(path).map_err(|err| open_failure("file", &err))?;
    if is_refused(&canonical) {
        return Err(Failure::new(
            Code::Einval,
            "refusing to read from /proc, /sys or /dev",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(OFlag::O_NONBLOCK.bits())
        .open(&canonical)
        .map_err(|err| open_failure("file", &err))?;
    let metadata = file.metadata().map_err(|err| read_failure(&err))?;
    if !metadata.is_file() {
        return Err(Failure::new(Code::Einval, "not a regular file"));
    }
    let data = read_range(&mut file, metadata.len(), command);
    if temporary {
        remove_if_temporary(&canonical, &temp_dirs());
    }
    data
}

fn read_range(file: &mut File, len: u64, command: &Command) -> Result<Vec<u8>, Failure> {
    let (offset, wanted) = range(len, command)?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| read_failure(&err))?;
    let mut data = Vec::with_capacity(wanted);
    file.take(wanted as u64)
        .read_to_end(&mut data)
        .map_err(|err| read_failure(&err))?;
    Ok(data)
}

fn range(len: u64, command: &Command) -> Result<(u64, usize), Failure> {
    let offset = u64::from(command.data_offset);
    let available = len.saturating_sub(offset);
    let wanted = match command.data_size {
        0 => available,
        size => available.min(u64::from(size)),
    };
    match usize::try_from(wanted) {
        Ok(wanted) if wanted <= MAX_DATA_LEN => Ok((offset, wanted)),
        _ => Err(too_big()),
    }
}

fn is_refused(path: &Path) -> bool {
    !path.starts_with(SHARED_MEMORY_DIR) && REFUSED_DIRS.iter().any(|dir| path.starts_with(dir))
}

fn temp_dirs() -> Vec<PathBuf> {
    [
        env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from(SHARED_MEMORY_DIR),
    ]
    .into_iter()
    .filter_map(|dir| fs::canonicalize(dir).ok())
    .collect()
}

fn remove_if_temporary(path: &Path, temp_dirs: &[PathBuf]) {
    let marked = path
        .as_os_str()
        .as_bytes()
        .windows(TEMP_FILE_MARKER.len())
        .any(|window| window == TEMP_FILE_MARKER);
    if marked && temp_dirs.iter().any(|dir| path.starts_with(dir)) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(not(target_os = "android"))]
fn read_shared_memory(name: &[u8], command: &Command) -> Result<Vec<u8>, Failure> {
    use std::num::NonZeroUsize;

    use nix::sys::mman::{mmap, munmap, shm_open, shm_unlink, MapFlags, ProtFlags};
    use nix::sys::stat::Mode;

    let name = OsStr::from_bytes(name);
    let file = File::from(
        shm_open(name, OFlag::O_RDONLY, Mode::empty())
            .map_err(|errno| open_failure("shared memory", &io::Error::from(errno)))?,
    );
    let _ = shm_unlink(name);
    let len = file.metadata().map_err(|err| read_failure(&err))?.len();
    let (offset, wanted) = range(len, command)?;
    let Some(mapped_len) = usize::try_from(len).ok().and_then(NonZeroUsize::new) else {
        return Ok(Vec::new());
    };
    if wanted == 0 {
        return Ok(Vec::new());
    }
    let mapped = unsafe {
        mmap(
            None,
            mapped_len,
            ProtFlags::PROT_READ,
            MapFlags::MAP_SHARED,
            &file,
            0,
        )
    }
    .map_err(|errno| read_failure(&io::Error::from(errno)))?;
    let start = offset as usize;
    let data = unsafe {
        std::slice::from_raw_parts(mapped.as_ptr().cast::<u8>(), mapped_len.get())
            [start..start + wanted]
            .to_vec()
    };
    let _ = unsafe { munmap(mapped, mapped_len.get()) };
    Ok(data)
}

#[cfg(target_os = "android")]
fn read_shared_memory(_: &[u8], _: &Command) -> Result<Vec<u8>, Failure> {
    Err(Failure::new(
        Code::Einval,
        "shared memory is not supported on Android",
    ))
}

fn open_failure(what: &str, err: &io::Error) -> Failure {
    let code = match err.kind() {
        io::ErrorKind::NotFound => Code::Enoent,
        _ => Code::Ebadf,
    };
    Failure::new(code, format!("can't open the {what}: {err}"))
}

fn read_failure(err: &io::Error) -> Failure {
    Failure::new(Code::Ebadf, format!("can't read the data: {err}"))
}

fn too_big() -> Failure {
    Failure::new(
        Code::Efbig,
        format!("the data is larger than {} MiB", MAX_DATA_LEN / 1024 / 1024),
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use base64::engine::general_purpose::STANDARD;

    use super::*;

    fn command(control: &str, payload: &[u8]) -> Command {
        let mut command = Command::parse(control.as_bytes()).unwrap();
        command.payload = payload.to_vec();
        command
    }

    fn encoded(control: &str, data: &[u8]) -> Command {
        command(control, STANDARD.encode(data).as_bytes())
    }

    fn complete(step: Step) -> Transmission {
        match step {
            Step::Complete(transmission) => transmission,
            other => panic!("expected a complete transmission, got {other:?}"),
        }
    }

    fn send(control: &str, data: &[u8]) -> Result<Image, Failure> {
        match Transmissions::default().receive(Buffer::Main, encoded(control, data)) {
            Step::Complete(transmission) => load(&transmission.command, transmission.data),
            Step::Failed(_, failure) => Err(failure),
            other => panic!("expected one step, got {other:?}"),
        }
    }

    fn code(control: &str, data: &[u8]) -> Code {
        send(control, data).unwrap_err().code
    }

    fn path_of(path: &Path) -> &[u8] {
        path.as_os_str().as_bytes()
    }

    fn pixels(len: usize) -> Vec<u8> {
        (0..len).map(|byte| (byte * 7 % 251) as u8).collect()
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(&13_u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 1, 2, 3, 4, 0, 0, 0, 0]);
        png
    }

    #[test]
    fn chunked_direct_data_survives_awkward_base64_boundaries() {
        for (control, data) in [
            ("a=t,f=24,s=10,v=10,i=1", pixels(300)),
            ("a=T,f=32,s=1,v=1,I=2", pixels(4)),
            ("a=q,f=24,s=1,v=1,i=3", pixels(3)),
        ] {
            let first = Command::parse(control.as_bytes()).unwrap();
            let text = STANDARD.encode(&data);
            for size in [1, 2, 3, 5, 6, 7, 13, 4096] {
                let mut transmissions = Transmissions::default();
                let chunks: Vec<&[u8]> = text.as_bytes().chunks(size).collect();
                let (last, rest) = chunks.split_last().unwrap();
                for (index, chunk) in rest.iter().enumerate() {
                    let control = if index == 0 {
                        format!("{control},m=1")
                    } else {
                        "m=1".to_owned()
                    };
                    let step = transmissions.receive(Buffer::Main, command(&control, chunk));
                    assert_eq!(step, Step::Waiting, "chunk {index} of size {size}");
                }
                let end = if rest.is_empty() { control } else { "m=0" };
                let done = complete(transmissions.receive(Buffer::Main, command(end, last)));
                assert_eq!(done.data, data, "chunks of {size} for {control}");
                assert_eq!(
                    (done.command.action, done.command.id, done.command.number),
                    (first.action, first.id, first.number)
                );
                assert!(!done.command.more);
            }
        }
    }

    #[test]
    fn later_chunks_only_change_more_and_quiet() {
        let mut transmissions = Transmissions::default();
        let first = transmissions.receive(Buffer::Main, encoded("i=5,f=24,s=1,v=1,m=1", b"ab"));
        assert_eq!(first, Step::Waiting);
        let done =
            complete(transmissions.receive(Buffer::Main, encoded("i=9,f=100,q=1,a=q,m=0", b"c")));
        assert_eq!((done.command.id, done.command.format), (5, 24));
        assert_eq!(done.command.quiet, 1);
        assert_eq!(done.data, b"abc");

        transmissions.receive(Buffer::Main, encoded("i=6,q=2,m=1", b"a"));
        let done = complete(transmissions.receive(Buffer::Main, encoded("m=0", b"b")));
        assert_eq!(done.command.quiet, 2);
    }

    #[test]
    fn each_screen_buffer_has_its_own_transmission_in_progress() {
        let mut transmissions = Transmissions::default();
        transmissions.receive(Buffer::Main, encoded("i=1,m=1", b"main "));
        let alt = complete(transmissions.receive(Buffer::Alt, encoded("i=2", b"alt")));
        assert_eq!(
            (alt.command.id, alt.data.as_slice()),
            (2, b"alt".as_slice())
        );
        let main = complete(transmissions.receive(Buffer::Main, encoded("m=0", b"data")));
        assert_eq!(
            (main.command.id, main.data.as_slice()),
            (1, b"main data".as_slice())
        );
    }

    #[test]
    fn a_command_with_another_medium_starts_a_new_transmission() {
        let mut transmissions = Transmissions::default();
        transmissions.receive(Buffer::Main, encoded("i=1,m=1", b"lost"));
        let file = complete(transmissions.receive(Buffer::Main, encoded("i=2,t=f", b"/x")));
        assert_eq!((file.command.id, file.command.medium), (2, b'f'));
        let fresh = complete(transmissions.receive(Buffer::Main, encoded("m=0", b"new")));
        assert_eq!(
            (fresh.command.id, fresh.data.as_slice()),
            (0, b"new".as_slice())
        );
    }

    #[test]
    fn after_an_error_the_rest_of_the_transmission_is_swallowed() {
        let mut transmissions = Transmissions::default();
        assert_eq!(
            transmissions.receive(Buffer::Main, encoded("i=1,f=24,s=1,v=1,m=1", b"a")),
            Step::Waiting
        );
        let Step::Failed(failed, failure) =
            transmissions.receive(Buffer::Main, command("m=1", b"!!!!"))
        else {
            panic!("bad base64 must fail");
        };
        assert_eq!((failed.id, failure.code), (1, Code::Einval));
        assert_eq!(
            transmissions.receive(Buffer::Main, encoded("m=1", b"b")),
            Step::Swallowed
        );
        assert_eq!(
            transmissions.receive(Buffer::Main, encoded("m=0", b"c")),
            Step::Swallowed
        );
        let next = complete(transmissions.receive(Buffer::Main, encoded("i=2", b"ok")));
        assert_eq!(next.command.id, 2);

        let Step::Failed(failed, failure) =
            transmissions.receive(Buffer::Main, encoded("i=3,I=4,m=1", b"a"))
        else {
            panic!("an id and a number together must fail");
        };
        assert_eq!(
            (failed.id, failed.number, failure.code),
            (3, 4, Code::Einval)
        );
        assert_eq!(
            transmissions.receive(Buffer::Main, encoded("m=0", b"b")),
            Step::Swallowed
        );
    }

    #[test]
    fn raw_pixels_need_a_size_and_enough_data() {
        let rgb = send("f=24,s=2,v=1", &pixels(7)).unwrap();
        assert_eq!(
            rgb,
            Image {
                width: 2,
                height: 1,
                format: ImageFormat::Rgb24,
                compressed: false,
                bytes: pixels(6),
                decoded_len: 6,
            }
        );
        let rgba = send("s=1,v=2", &pixels(8)).unwrap();
        assert_eq!(
            (rgba.format, rgba.decoded_len, rgba.bytes.len()),
            (ImageFormat::Rgba32, 8, 8)
        );

        let short = send("f=32,s=2,v=2", &pixels(15)).unwrap_err();
        assert_eq!(short.code, Code::Enodata);
        assert_eq!(short.message, "insufficient image data: 15 < 16");
        assert_eq!(code("f=24,s=0,v=1", &pixels(3)), Code::Einval);
        assert_eq!(code("f=24,s=1", &pixels(3)), Code::Einval);
        assert_eq!(code("f=24,s=10001,v=1", &pixels(3)), Code::Efbig);
        assert_eq!(code("f=99,s=1,v=1", &pixels(4)), Code::Einval);
    }

    #[test]
    fn a_png_is_kept_as_sent_with_the_size_from_its_header() {
        let sent = png(640, 480);
        let image = send("f=100,s=1,v=1", &sent).unwrap();
        assert_eq!(
            image,
            Image {
                width: 640,
                height: 480,
                format: ImageFormat::Png,
                compressed: false,
                bytes: sent.clone(),
                decoded_len: 640 * 480 * 4,
            }
        );
        assert_eq!(image.clone().packed(), image);

        assert_eq!(code("f=100", &sent[..23]), Code::Ebadpng);
        assert_eq!(code("f=100", b"GIF89a, not a png at all"), Code::Ebadpng);
        assert_eq!(code("f=100", &png(0, 5)), Code::Ebadpng);
        assert_eq!(code("f=100", &png(10_001, 5)), Code::Efbig);
        let mut not_ihdr = png(1, 1);
        not_ihdr[12..16].copy_from_slice(b"IDAT");
        assert_eq!(code("f=100", &not_ihdr), Code::Ebadpng);
    }

    #[test]
    fn zlib_data_is_kept_compressed_and_only_inflated_to_check_it() {
        let raw = pixels(4 * 30 * 20);
        let compressed = compress_to_vec_zlib(&raw, 6);
        let image = send("f=32,s=30,v=20,o=z", &compressed).unwrap();
        assert_eq!(
            image,
            Image {
                width: 30,
                height: 20,
                format: ImageFormat::Rgba32,
                compressed: true,
                bytes: compressed.clone(),
                decoded_len: raw.len(),
            }
        );
        assert_eq!(image.clone().packed(), image);
        let short = send("f=32,s=30,v=21,o=z", &compressed).unwrap_err();
        assert_eq!(short.code, Code::Enodata);

        let sent = png(3, 4);
        let image = send("f=100,o=z", &compress_to_vec_zlib(&sent, 6)).unwrap();
        assert_eq!((image.width, image.height, image.compressed), (3, 4, true));
        assert_eq!(
            code("f=100,o=z", &compress_to_vec_zlib(&sent[..20], 6)),
            Code::Ebadpng
        );

        assert_eq!(code("f=32,s=1,v=1,o=z", b"not zlib data"), Code::Einval);
        assert_eq!(code("f=32,s=1,v=1,o=x", &pixels(4)), Code::Einval);
    }

    #[test]
    fn raw_pixels_are_deflated_once_for_storage() {
        let raw = pixels(3 * 50 * 50);
        let packed = send("f=24,s=50,v=50", &raw).unwrap().packed();
        assert!(packed.compressed);
        assert_eq!(packed.decoded_len, raw.len());
        assert_eq!(
            miniz_oxide::inflate::decompress_to_vec_zlib(&packed.bytes).unwrap(),
            raw
        );
    }

    #[test]
    fn a_file_is_read_from_its_offset_for_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.rgb");
        fs::write(
            &path,
            [b"HEADER".as_slice(), &pixels(6), b"TRAILER"].concat(),
        )
        .unwrap();
        let image = send("t=f,f=24,s=2,v=1,O=6,S=6", path_of(&path)).unwrap();
        assert_eq!(image.bytes, pixels(6));
        let whole = send("t=f,f=24,s=2,v=1", path_of(&path)).unwrap();
        assert_eq!(whole.bytes, b"HEADER");
        assert_eq!(code("t=f,f=24,s=2,v=1,O=15", path_of(&path)), Code::Enodata);
        assert_eq!(code("t=f,f=24,s=2,v=1,O=99", path_of(&path)), Code::Enodata);
        assert!(path.exists());
    }

    #[test]
    fn files_that_are_missing_odd_or_too_big_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.png");
        assert_eq!(code("t=f,f=100", path_of(&missing)), Code::Enoent);
        assert_eq!(code("t=t,f=100", path_of(&missing)), Code::Enoent);
        assert_eq!(code("t=f,f=100", b"relative/image.png"), Code::Einval);
        assert_eq!(code("t=f,f=100", path_of(dir.path())), Code::Einval);

        let looped = dir.path().join("loop");
        symlink(&looped, &looped).unwrap();
        assert_eq!(code("t=f,f=100", path_of(&looped)), Code::Ebadf);

        let huge = dir.path().join("huge.rgba");
        File::create(&huge)
            .unwrap()
            .set_len(MAX_DATA_LEN as u64 + 1)
            .unwrap();
        assert_eq!(code("t=f,f=32,s=1,v=1", path_of(&huge)), Code::Efbig);
        let sized = format!("t=f,f=32,s=1,v=1,S={}", MAX_DATA_LEN + 1);
        assert_eq!(code(&sized, path_of(&huge)), Code::Efbig);
        let fits = send("t=f,f=32,s=1,v=1,S=4,O=4096", path_of(&huge)).unwrap();
        assert_eq!(fits.bytes, [0; 4]);

        assert_eq!(code("t=x,f=100", b"/"), Code::Einval);
    }

    #[test]
    fn proc_sys_and_dev_are_refused_even_through_a_symlink() {
        for path in [
            "/proc/self/status",
            "/sys/kernel/hostname",
            "/dev/null",
            "/dev/zero",
        ] {
            if Path::new(path).exists() {
                let failure = send("t=f,f=100", path.as_bytes()).unwrap_err();
                assert_eq!(failure.code, Code::Einval, "{path}");
                assert!(failure.message.contains("refusing"), "{path}: {failure:?}");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("status.png");
        symlink("/proc/self/status", &link).unwrap();
        assert_eq!(code("t=f,f=100", path_of(&link)), Code::Einval);
    }

    #[test]
    fn a_temporary_file_is_deleted_only_in_a_temp_dir_with_the_marker_in_its_path() {
        let marked = tempfile::Builder::new()
            .prefix("tty-graphics-protocol-")
            .tempdir()
            .unwrap();
        let unmarked = tempfile::tempdir().unwrap();
        let image = png(2, 2);

        let deleted = marked.path().join("image.png");
        fs::write(&deleted, &image).unwrap();
        assert_eq!(send("t=t,f=100", path_of(&deleted)).unwrap().bytes, image);
        assert!(!deleted.exists());

        let named = unmarked.path().join("tty-graphics-protocol-image.png");
        fs::write(&named, &image).unwrap();
        send("t=t,f=100", path_of(&named)).unwrap();
        assert!(!named.exists());

        let kept = unmarked.path().join("image.png");
        fs::write(&kept, &image).unwrap();
        send("t=t,f=100", path_of(&kept)).unwrap();
        assert!(kept.exists());

        let file = marked.path().join("file.png");
        fs::write(&file, &image).unwrap();
        send("t=f,f=100", path_of(&file)).unwrap();
        assert!(file.exists());

        let outside = fs::canonicalize(&file).unwrap();
        remove_if_temporary(&outside, &[fs::canonicalize(unmarked.path()).unwrap()]);
        assert!(outside.exists());
        remove_if_temporary(&outside, &temp_dirs());
        assert!(!outside.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_memory_is_read_and_unlinked() {
        use nix::sys::mman::shm_open;
        use nix::sys::stat::Mode;

        static OBJECTS: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "/amux-graphics-test-{}-{}",
            std::process::id(),
            OBJECTS.fetch_add(1, Ordering::Relaxed)
        );
        let create = OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR;
        let data = [b"skip".as_slice(), &pixels(12)].concat();
        let mut object =
            File::from(shm_open(name.as_str(), create, Mode::S_IRUSR | Mode::S_IWUSR).unwrap());
        object.write_all(&data).unwrap();
        drop(object);

        let image = send("t=s,f=24,s=2,v=2,O=4,S=12", name.as_bytes()).unwrap();
        assert_eq!(image.bytes, pixels(12));
        let gone = shm_open(name.as_str(), OFlag::O_RDONLY, Mode::empty()).unwrap_err();
        assert_eq!(gone, nix::errno::Errno::ENOENT);
        assert_eq!(code("t=s,f=24,s=2,v=2", name.as_bytes()), Code::Enoent);
    }
}
