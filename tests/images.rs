mod common;

use std::fs;
use std::path::{Path, PathBuf};

use amux::protocol::{
    CellPixels, ClientMessage, ClientTerminal, ImageFormat, ImageOp, ServerMessage,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use common::{settled_pair, terminal_log, Listing, TestClient, TestServer, DETACH};
use tokio::task::block_in_place;

const PLACEHOLDER: char = '\u{10eeee}';
const CELL: CellPixels = CellPixels {
    width: 10,
    height: 20,
};
const KITTY: ClientTerminal = ClientTerminal {
    graphics: true,
    cell_pixels: Some(CELL),
};
const PROBE: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[>q\x1b[16t\x1b[c";
const KITTY_ANSWERS: &str =
    "\x1b_Gi=31;OK\x1b\\\x1bP>|kitty(0.35.0)\x1b\\\x1b[6;20;10t\x1b[?62;22c";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.png")
}

fn show(keys: &str, payload: &str) -> String {
    format!("printf '\\033_G{keys},q=2;%s\\033\\\\' \"$({payload} | base64 | tr -d '\\n')\"\r")
}

fn show_png(path: &Path) -> String {
    let quoted = shell_words::quote(&path.display().to_string()).into_owned();
    show("a=T,f=100,i=1", &format!("cat {quoted}"))
}

fn key_colour(key: u32) -> vt100::Color {
    let [_, red, green, blue] = key.to_be_bytes();
    vt100::Color::Rgb(red, green, blue)
}

fn placeholders(screen: &vt100::Screen) -> Vec<(u16, u16, vt100::Color)> {
    let (rows, cols) = screen.size();
    (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (row, col)))
        .filter_map(|(row, col)| {
            let cell = screen.cell(row, col)?;
            cell.contents()
                .starts_with(PLACEHOLDER)
                .then(|| (row, col, cell.fgcolor()))
        })
        .collect()
}

async fn images_until(
    client: &mut TestClient,
    what: &str,
    done: impl Fn(&[ImageOp], &vt100::Screen) -> bool,
) -> Vec<ImageOp> {
    let mut ops = Vec::new();
    while !done(&ops, client.screen()) {
        match client.recv().await {
            Some(ServerMessage::Image(op)) => ops.push(op),
            Some(ServerMessage::Output(_)) => {}
            other => panic!("expected output or images while waiting for {what}, got {other:?}"),
        }
    }
    ops
}

async fn switch(client: &mut TestClient, target: &str) -> Vec<ImageOp> {
    client
        .send(ClientMessage::Switch(target.parse().unwrap()))
        .await;
    let mut ops = Vec::new();
    loop {
        match client.recv().await {
            Some(ServerMessage::Attached(attached)) if attached.session == target => return ops,
            Some(ServerMessage::Image(op)) => ops.push(op),
            Some(ServerMessage::Output(_)) => {}
            other => panic!("expected to switch to {target}, got {other:?}"),
        }
    }
}

fn uploaded(ops: &[ImageOp]) -> Option<(u32, Vec<u8>, (u16, u16))> {
    let mut data = Vec::new();
    for op in ops {
        match op {
            ImageOp::Transmit {
                data: chunk, last, ..
            } => {
                data.extend_from_slice(chunk);
                if !last {
                    continue;
                }
            }
            ImageOp::Place { key, cols, rows } => return Some((*key, data, (*cols, *rows))),
            ImageOp::Delete { .. } => {}
        }
    }
    None
}

async fn kitty_session(server: &TestServer, name: &str) -> TestClient {
    let mut client = server.client().await;
    client.new_session(Some(name)).await;
    client.wait_for_text("$").await;
    client.send(ClientMessage::Terminal(KITTY)).await;
    block_in_place(|| server.wait_for_log(&terminal_log(name, KITTY)));
    client
}

async fn show_tiny_png(client: &mut TestClient) -> u32 {
    client.type_text(&show_png(&fixture())).await;
    let ops = images_until(client, "the image and its placeholders", |ops, screen| {
        uploaded(ops).is_some() && placeholders(screen).len() == 4
    })
    .await;
    let [ImageOp::Transmit {
        key,
        format,
        width,
        height,
        compressed,
        total,
        data,
        last,
    }, ImageOp::Place {
        key: placed,
        cols,
        rows,
    }] = &ops[..]
    else {
        panic!("expected one transmission and a placement, got {ops:?}");
    };
    let png = fs::read(fixture()).unwrap();
    assert_eq!(
        (*format, *width, *height, *compressed, *last),
        (ImageFormat::Png, 20, 40, false, true)
    );
    assert_eq!(*total as usize, png.len());
    assert_eq!(*data, png);
    assert_eq!((*placed, *cols, *rows), (*key, 2, 2));
    *key
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kitty_client_gets_the_png_and_placeholder_cells_in_its_colour() {
    let server = TestServer::start();
    let mut client = kitty_session(&server, "s").await;
    let key = show_tiny_png(&mut client).await;

    let cells = placeholders(client.screen());
    let (top, left, _) = cells[0];
    assert_eq!(
        cells,
        [
            (top, left, key_colour(key)),
            (top, left + 1, key_colour(key)),
            (top + 1, left, key_colour(key)),
            (top + 1, left + 1, key_colour(key)),
        ]
    );
    let contents = client.screen().cell(top + 1, left + 1).unwrap().contents();
    let expected: String = [PLACEHOLDER, '\u{30d}', '\u{30d}', '\u{305}']
        .iter()
        .collect();
    assert_eq!(contents, expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_without_graphics_sees_no_placeholders() {
    let server = TestServer::start();
    let mut kitty = kitty_session(&server, "s").await;
    show_tiny_png(&mut kitty).await;
    let (top, left, _) = placeholders(kitty.screen())[0];

    let mut plain = server.client().await;
    plain.attach(Some("s")).await;
    plain.wait_for_text("$").await;
    plain.type_text("echo plain-$((6*7))\r").await;
    plain.wait_for_text("plain-42").await;
    assert!(placeholders(plain.screen()).is_empty());
    for (row, col) in [(top, left), (top + 1, left + 1)] {
        let cell = plain.screen().cell(row, col).unwrap();
        assert!(!cell.has_contents(), "({row}, {col}): {cell:?}");
    }
    plain.detach().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_scrolled_away_is_deleted_from_the_client() {
    let server = TestServer::start();
    let mut client = kitty_session(&server, "s").await;
    let key = show_tiny_png(&mut client).await;

    client.type_text("seq 1 100\r").await;
    let ops = images_until(&mut client, "the image to be deleted", |ops, screen| {
        ops.contains(&ImageOp::Delete { key }) && placeholders(screen).is_empty()
    })
    .await;
    assert_eq!(ops, [ImageOp::Delete { key }]);
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_sessions_uploads_the_image_again() {
    let server = TestServer::start();
    let mut other = server.client().await;
    other.new_session(Some("t")).await;
    other.detach().await;
    let mut client = kitty_session(&server, "s").await;
    let key = show_tiny_png(&mut client).await;

    let while_away = switch(&mut client, "t").await;
    assert!(uploaded(&while_away).is_none(), "{while_away:?}");
    client.reset_screen();
    let mut ops = switch(&mut client, "s").await;
    ops.extend(
        images_until(&mut client, "the image to come back", |ops, screen| {
            uploaded(ops).is_some() && placeholders(screen).len() == 4
        })
        .await,
    );
    let (uploaded_key, data, size) = uploaded(&ops).unwrap();
    assert_eq!((uploaded_key, size), (key, (2, 2)));
    assert_eq!(data, fs::read(fixture()).unwrap());
}

fn noise(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_be_bytes()[0]
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_client_gets_the_image_in_order_and_in_peer_sized_chunks() {
    let [a, b] = settled_pair(
        TestServer::builder().name("a"),
        TestServer::builder().name("b"),
    );
    let mut host = b.client().await;
    host.new_session(Some("s")).await;
    host.wait_for_text("$").await;
    host.detach().await;
    block_in_place(|| a.wait_for_ls("s on b", |listing: &Listing| listing.sessions("b") == ["s"]));

    let mut client = a.client().await;
    client.attach_to("s@b").await;
    client.wait_for_text("$").await;
    client.send(ClientMessage::Terminal(KITTY)).await;
    block_in_place(|| b.wait_for_log(&terminal_log("s", KITTY)));

    let pixels = noise(160 * 160 * 4);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noise.rgba");
    fs::write(&path, &pixels).unwrap();
    let quoted = shell_words::quote(&path.display().to_string()).into_owned();
    client
        .type_text(&show(
            "a=T,f=32,s=160,v=160,t=f,i=1",
            &format!("printf %s {quoted}"),
        ))
        .await;
    let ops = images_until(&mut client, "the remote image", |ops, _| {
        uploaded(ops).is_some()
    })
    .await;

    let chunks: Vec<(usize, bool)> = ops
        .iter()
        .filter_map(|op| match op {
            ImageOp::Transmit { data, last, .. } => Some((data.len(), *last)),
            _ => None,
        })
        .collect();
    let (key, data, size) = uploaded(&ops).unwrap();
    assert_eq!(chunks[0], (64 * 1024, false));
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    assert_eq!(chunks[1], (data.len() - 64 * 1024, true));
    assert!(matches!(ops.last(), Some(ImageOp::Place { .. })), "{ops:?}");
    assert!(matches!(
        ops[0],
        ImageOp::Transmit {
            format: ImageFormat::Rgba32,
            width: 160,
            height: 160,
            compressed: true,
            ..
        }
    ));
    assert_eq!(size, (16, 8));
    assert_eq!(
        miniz_oxide::inflate::decompress_to_vec_zlib(&data).unwrap(),
        pixels
    );
    client
        .wait_for_screen("the remote placeholders", |screen| {
            placeholders(screen)
                .iter()
                .any(|(_, _, colour)| *colour == key_colour(key))
        })
        .await;
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn placed_key(raw: &[u8]) -> Option<u32> {
    let intro = b"\x1b_Ga=p,U=1,i=";
    let at = raw
        .windows(intro.len())
        .position(|window| window == intro)?;
    let digits: String = raw[at + intro.len()..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .map(|byte| char::from(*byte))
        .collect();
    digits.parse().ok()
}

#[test]
fn a_terminal_that_answers_the_probe_gets_kitty_graphics_commands() {
    let server = TestServer::start();
    let mut terminal = server.terminal(&["new", "-s", "s"]);
    terminal.wait_for_raw("the graphics probe", |raw| contains(raw, PROBE));
    terminal.type_text(KITTY_ANSWERS);
    server.wait_for_log(&terminal_log("s", KITTY));
    terminal.wait_for_text("$");

    terminal.type_text(&show_png(&fixture()));
    let raw = terminal.wait_for_raw("the kitty upload", |raw| placed_key(raw).is_some());
    let key = placed_key(raw).unwrap();
    let png = STANDARD.encode(fs::read(fixture()).unwrap());
    let transmit = format!("\x1b_Ga=t,i={key},f=100,s=20,v=40,q=2,m=0;{png}\x1b\\");
    let place = format!("\x1b_Ga=p,U=1,i={key},c=2,r=2,q=2\x1b\\");
    let transmitted = raw
        .windows(transmit.len())
        .position(|window| window == transmit.as_bytes());
    let placed = raw
        .windows(place.len())
        .position(|window| window == place.as_bytes());
    assert!(
        transmitted.is_some() && transmitted < placed,
        "{:?}",
        String::from_utf8_lossy(raw)
    );
    terminal.wait_for_screen("the placeholders", |screen| placeholders(screen).len() == 4);
    assert!(placeholders(terminal.screen())
        .iter()
        .all(|(_, _, colour)| *colour == key_colour(key)));

    terminal.type_text(DETACH);
    terminal.wait_for_exit();
    let delete = format!("\x1b_Ga=d,d=I,i={key},q=2\x1b\\");
    terminal.wait_for_raw("the images to be cleaned up", |raw| {
        contains(raw, delete.as_bytes())
    });
}
