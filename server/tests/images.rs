//! Pictures and links through the real server (POL-01, D-03, D-04, D-05): a real shell in a real pane of the real worker prints a
//! Kitty picture or an OSC 8 link, and a real WebSocket carries the placement, the pixels and the link spans to the person; a
//! hostile request for pixels is answered without them. The desktop and the server share the host's table, so what is shown here
//! is the same line the desktop uses (D-03); no browser is involved.
//!
//! Every positive case has its control in the same test (the screen before the sequence, or the same command without it), and
//! every wait is a read until the screen shows a marker, never a fixed sleep. Needs the `in-process-workers` feature, and the
//! page must have been built (`npm --prefix app run build`).
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/parity_env.rs"]
mod parity_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::time::Duration;

use parity_env::{
    CLEAR, Line, Probed, cell_flags, fresh, kitty_picture_command, last_images, osc8_command, rows_of, split_and_start, start_focused_pane,
    type_into, until_screen,
};
use serde_json::{Value, json};
use ws_env::{Person, WsOptions};

/// How long a test waits for a screen.
const BOUND: Duration = Duration::from_secs(45);

/// The image id the pictures use.
const ID: u32 = 9;

/// Two pixels of straight RGBA: red, then green.
const RED_GREEN: [u8; 8] = [255, 0, 0, 255, 0, 255, 0, 255];

/// The picture of the other person: blue, then white (never the bytes of `RED_GREEN`).
const BLUE_WHITE: [u8; 8] = [0, 0, 255, 255, 255, 255, 255, 255];

/// A word the program prints beside its picture; no answer to a refused request may carry it.
const SECRET: &str = "hw-program-text-7d1";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Waits until a row of the pane's screen is exactly `marker` (a line of its own: the echo of the typed command holds the word
/// inside a longer line).
async fn until_marker(line: &mut Line, pane: u64, marker: &str) {
    let seen = until_screen(line, pane, BOUND, |rows| rows.iter().any(|r| r.trim() == marker)).await;
    assert!(seen, "{marker} never appeared on the screen of the pane");
}

/// Types the command, then a marker line, and waits for the marker.
async fn run_and_wait(line: &mut Line, pane: u64, command: &str, marker: &str) {
    type_into(line, pane, &format!("{command}; echo {marker}\r")).await;
    until_marker(line, pane, marker).await;
}

/// A signed-in person with a pane (after `splits` further splits, so that its id is higher) that printed `pixels` as image `ID`
/// (2 by 1 pixels over 2 by 1 cells) at the top left of a cleared screen, followed by `SECRET`. Returns the socket, the pane and
/// the placement of the picture.
async fn pane_with_picture(probed: &Probed, person: &Person, pixels: &[u8; 8], splits: usize) -> (Line, u64, Vec<Value>) {
    let mut line = fresh(probed, person).await;
    let mut pane = start_focused_pane(&mut line).await;
    for _ in 0..splits {
        pane = split_and_start(&mut line).await;
    }
    run_and_wait(&mut line, pane, &format!("{CLEAR}; {}", kitty_picture_command(ID, 2, 1, pixels, 2, 1)), SECRET).await;
    let list = last_images(&line, pane).expect("a frame carried the list of images");
    assert_eq!(list.len(), 1, "one picture was printed: {list:?}");
    let placement = list[0].as_array().expect("a placement is an array").clone();
    (line, pane, placement)
}

fn read_args(pane: u64, placement: &[Value], offset: u64) -> Value {
    json!({"pane": pane, "image": placement[0], "generation": placement[1], "offset": offset})
}

/// The error text of a refused request; panics when the answer is not an error.
fn refusal(answer: &Result<Vec<u8>, Value>, what: &str) -> String {
    match answer {
        Err(value) => value["err"].as_str().unwrap_or_else(|| panic!("{what}: an error string was due: {value}")).to_owned(),
        Ok(bytes) => panic!("{what}: pixels came back ({} bytes: {})", bytes.len(), hex(bytes)),
    }
}

// ---- the placement and the pixels ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_picture_is_one_placement_in_a_frame_and_its_pixels_are_a_binary_answer_that_starts_with_the_call_id() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;

    // The control: the screen cleared by a command that prints no picture has no list of images and no pixels to read.
    run_and_wait(&mut line, pane, CLEAR, "hw-before-the-picture").await;
    assert!(last_images(&line, pane).is_none(), "a screen with no picture has no images key in any frame");
    let before = line.raw_call("image_data", &json!({"pane": pane, "image": ID, "generation": 1, "offset": 0})).await;
    assert_eq!(refusal(&before, "the picture that is not there yet"), "no such image");

    run_and_wait(&mut line, pane, &format!("{CLEAR}; {}; echo {SECRET}", kitty_picture_command(ID, 2, 1, &RED_GREEN, 2, 1)), "hw-after")
        .await;
    let list = last_images(&line, pane).expect("the picture is in a frame");
    assert_eq!(list.len(), 1, "one picture: {list:?}");
    let placement = list[0].as_array().expect("one array");
    assert_eq!(placement.len(), 16, "a placement is sixteen numbers: {placement:?}");
    assert!(placement.iter().all(Value::is_number), "{placement:?}");
    // id, generation, layer, z, column, row, grid columns, grid rows, x offset, y offset, pixel width, pixel height, source ...
    assert_eq!(placement[0], json!(ID), "the id the program chose");
    assert!(placement[1].as_u64().is_some_and(|g| g > 0), "a generation: {}", placement[1]);
    assert_eq!(placement[2], json!(2), "a picture with no z is above the text");
    assert_eq!((placement[4].as_i64(), placement[5].as_i64()), (Some(0), Some(0)), "at the top left after the clear");
    assert_eq!((placement[6].as_u64(), placement[7].as_u64()), (Some(2), Some(1)), "two cells by one");
    assert_eq!(
        (placement[10].as_u64(), placement[11].as_u64()),
        (Some(16), Some(16)),
        "the area in pixels is two cells of 8 by 16 by one row"
    );
    assert_eq!(
        (placement[14].as_u64(), placement[15].as_u64()),
        (Some(2), Some(1)),
        "the source is the whole picture, two pixels by one: {placement:?}"
    );

    // No frame event carries the pixels: they are pulled (the base64 of the two pixels and the word of the program's text).
    let coded = parity_env::base64_std(&RED_GREEN);
    for event in line.events_named("frame") {
        let text = event.to_string();
        assert!(!text.contains(&coded), "a frame holds the pixels: {text}");
    }

    // The pixels: a binary message whose first eight bytes are the call id (`raw_call` matches them and takes them off), then
    // width, height, total length and offset as four big-endian u32, then the straight RGBA bytes.
    let bytes = line.raw_call("image_data", &read_args(pane, placement, 0)).await.expect("the pixels");
    println!("Binary answer (after the 8-byte call id): {}", hex(&bytes));
    assert_eq!(bytes[..16], [0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 8, 0, 0, 0, 0], "width 2, height 1, total 8, offset 0");
    assert_eq!(bytes[16..], RED_GREEN, "the two pixels, red then green");
    assert_eq!(bytes.len(), 24);

    // A read that starts inside the picture answers the rest and says where it starts.
    let rest = line.raw_call("image_data", &read_args(pane, placement, 4)).await.expect("the rest");
    assert_eq!(rest[..16], [0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 8, 0, 0, 0, 4], "the same picture, offset 4");
    assert_eq!(rest[16..], RED_GREEN[4..], "the green pixel");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_without_the_sequence_leaves_no_images_and_no_links_in_any_frame() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;
    // The same shape as the two positive tests, without the escape sequences.
    run_and_wait(&mut line, pane, &format!("{CLEAR}; printf 'docs\\n'"), "hw-plain").await;
    for frame in line.frames_of(pane) {
        assert!(frame.get("images").is_none(), "an images key in a frame of a pane that printed no picture: {frame}");
        for row in frame["changed"].as_array().map(Vec::as_slice).unwrap_or_default() {
            assert!(row.get("links").is_none(), "a links key in a row of a pane that printed no link: {row}");
            assert!(cell_flags(row).iter().all(|f| f & 16 == 0), "the LINK flag on a row with no link: {row}");
        }
    }
    let rows = rows_of(&line, pane);
    assert!(rows.iter().any(|r| r["runs"].to_string().contains("docs")), "the text itself is on the screen (the control ran)");
}

// ---- hostile requests ---------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_does_not_name_the_picture_exactly_is_answered_without_pixels_and_without_the_programs_text() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut line, pane, placement) = pane_with_picture(&probed, &alice, &RED_GREEN, 0).await;
    let generation = placement[1].as_u64().expect("a generation");

    // The control: the exact request gets the pixels.
    let exact = line.raw_call("image_data", &read_args(pane, &placement, 0)).await.expect("the exact request is answered");
    assert_eq!(exact[16..], RED_GREEN);

    let coded = parity_env::base64_std(&RED_GREEN);
    let refused: Vec<(&str, Value)> = vec![
        ("the next generation", json!({"pane": pane, "image": ID, "generation": generation + 1, "offset": 0})),
        ("the generation before", json!({"pane": pane, "image": ID, "generation": generation.saturating_sub(1), "offset": 0})),
        ("generation 0", json!({"pane": pane, "image": ID, "generation": 0, "offset": 0})),
        ("an image id that was never used", json!({"pane": pane, "image": ID + 1, "generation": generation, "offset": 0})),
        ("image id 0", json!({"pane": pane, "image": 0, "generation": generation, "offset": 0})),
        ("a pane that does not exist", json!({"pane": 99_999, "image": ID, "generation": generation, "offset": 0})),
        ("a missing image argument", json!({"pane": pane, "generation": generation, "offset": 0})),
        ("a missing generation", json!({"pane": pane, "image": ID, "offset": 0})),
        ("a missing offset", json!({"pane": pane, "image": ID, "generation": generation})),
        ("a missing pane", json!({"image": ID, "generation": generation, "offset": 0})),
        ("an image id that is a string", json!({"pane": pane, "image": "9", "generation": generation, "offset": 0})),
        ("a generation that is a string", json!({"pane": pane, "image": ID, "generation": "1", "offset": 0})),
        ("a negative offset", json!({"pane": pane, "image": ID, "generation": generation, "offset": -1})),
        ("a fractional offset", json!({"pane": pane, "image": ID, "generation": generation, "offset": 0.5})),
        ("an image id above u32", json!({"pane": pane, "image": 4_294_967_296_u64 + u64::from(ID), "generation": generation, "offset": 0})),
        ("a null image id", json!({"pane": pane, "image": null, "generation": generation, "offset": 0})),
        ("an array for the arguments", json!([pane, ID, generation, 0])),
    ];
    let mut classes = Vec::new();
    for (what, args) in &refused {
        let answer = line.raw_call("image_data", args).await;
        let text = refusal(&answer, what);
        assert!(!text.contains(SECRET), "{what}: the error carries the program's text: {text}");
        assert!(!text.contains(&coded), "{what}: the error carries the pixels: {text}");
        classes.push(format!("{what}: error {text:?}"));
    }

    // An offset at or past the end is a read of nothing: the header, brought to the end, and no pixel byte.
    for offset in [8, 9, 24, 1 << 40, u64::MAX] {
        let answer =
            line.raw_call("image_data", &read_args(pane, &placement, offset)).await.unwrap_or_else(|e| panic!("offset {offset}: {e}"));
        assert_eq!(answer.len(), 16, "offset {offset}: the header and no pixel byte, got {}", hex(&answer));
        assert_eq!(answer[..12], [0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 8], "offset {offset}: the size of the picture");
        assert_eq!(answer[12..16], [0, 0, 0, 8], "offset {offset}: the offset in the header is brought to the end");
        classes.push(format!("offset {offset}: header only (16 bytes)"));
    }
    println!("Hostile: {}", classes.join("; "));

    // The pane is still alive and the exact request still works after all of it.
    let again = line.raw_call("image_data", &read_args(pane, &placement, 0)).await.expect("still answered");
    assert_eq!(again[16..], RED_GREEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn another_person_asking_for_the_pane_of_the_first_gets_an_error_and_never_the_first_persons_pixels() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let bob = probed.env.person("bob");
    // Alice's picture is in a pane with a higher id than anything Bob will have, so that her id names no pane of his.
    let (mut a, a_pane, a_placement) = pane_with_picture(&probed, &alice, &RED_GREEN, 3).await;
    let mut b = fresh(&probed, &bob).await;
    let b_pane = start_focused_pane(&mut b).await;
    let b_layout = b.ok("get_layout", &json!({})).await;
    let b_ids: Vec<u64> = b_layout["panes"].as_array().expect("panes").iter().filter_map(|p| p[0].as_u64()).collect();
    assert!(!b_ids.is_empty(), "Bob's layout lists his pane(s): {b_layout}");
    assert!(b_ids.contains(&b_pane), "the pane Bob started is in his layout: {b_ids:?}");
    assert!(!b_ids.contains(&a_pane), "Bob's panes {b_ids:?} must not include Alice's pane {a_pane}, or the test proves nothing");

    // The control: Alice reads her own picture with exactly this request.
    let args = read_args(a_pane, &a_placement, 0);
    let own = a.raw_call("image_data", &args).await.expect("Alice reads her own pixels");
    assert_eq!(own[16..], RED_GREEN);

    // Bob asks for it with the very same arguments: an error, no pixel.
    let foreign = b.raw_call("image_data", &args).await;
    let text = refusal(&foreign, "Bob asks for Alice's pane");
    assert_eq!(text, "no such image");

    // Bob prints a picture of his own with the same image id and other pixels; asking with Alice's pane id is still an error,
    // and his own pane answers his own bytes, never hers.
    run_and_wait(&mut b, b_pane, &format!("{CLEAR}; {}", kitty_picture_command(ID, 2, 1, &BLUE_WHITE, 2, 1)), "hw-bob-done").await;
    let b_list = last_images(&b, b_pane).expect("Bob's picture is in a frame");
    let b_placement = b_list[0].as_array().expect("an array").clone();
    let still = b.raw_call("image_data", &args).await;
    assert_eq!(refusal(&still, "Bob asks for Alice's pane after he has a picture of his own"), "no such image");
    let his = b.raw_call("image_data", &read_args(b_pane, &b_placement, 0)).await.expect("Bob reads his own");
    assert_eq!(his[16..], BLUE_WHITE, "Bob's pane answers Bob's pixels");
    assert_ne!(his[16..], own[16..]);
    // And the other way round: Alice cannot read Bob's pane either (her ids are higher than his, so this names no pane of hers).
    let back = a.raw_call("image_data", &read_args(b_pane, &b_placement, 0)).await;
    if b_pane != a_pane {
        let _ = refusal(&back, "Alice asks for Bob's pane");
    }
    println!("Hostile: another person, Alice's pane {a_pane} asked by Bob (his panes {b_ids:?}): error {text:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_switch_of_animations_is_accepted_both_ways_and_refuses_a_wrong_argument() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut line, pane, placement) = pane_with_picture(&probed, &alice, &RED_GREEN, 0).await;
    assert_eq!(line.ok("set_image_animation", &json!({"enabled": false})).await, Value::Null);
    assert_eq!(line.ok("set_image_animation", &json!({"enabled": true})).await, Value::Null);
    // A wrong argument is an error answer, not an accepted switch.
    for (what, args) in [("a string", json!({"enabled": "no"})), ("a missing argument", json!({})), ("a number", json!({"enabled": 0}))] {
        let id = line.send("set_image_animation", &args).await;
        let answer = line.wait(id, Duration::from_secs(10)).await.expect("an answer");
        assert!(answer.get("err").is_some() && answer.get("ok").is_none(), "{what} was accepted: {answer}");
    }
    // The switch does not take the picture away: the pixels are still there.
    let still = line.raw_call("image_data", &read_args(pane, &placement, 0)).await.expect("still there");
    assert_eq!(still[16..], RED_GREEN);
}

// ---- reattach -----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn after_the_socket_is_dropped_a_new_one_gets_the_placement_again_and_the_same_pixels() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (line, pane, placement) = pane_with_picture(&probed, &alice, &RED_GREEN, 0).await;
    drop(line);
    let key = alice.key;
    probed.env.eventually("the socket to detach", |env| env.supervisor.account_stats(key).sockets == 0).await;

    // What a page does after a reconnect: the layout, then a full frame of every pane.
    let mut again = fresh(&probed, &alice).await;
    assert!(last_images(&again, pane).is_none(), "the new socket has seen no frame yet (the control)");
    again.ok("get_layout", &json!({})).await;
    again.ok("request_full", &json!({"pane": pane})).await;
    let shown = again
        .until(BOUND, |events| {
            events.iter().any(|e| {
                e["event"] == "frame"
                    && e["payload"]["pane"].as_u64() == Some(pane)
                    && e["payload"]["full"] == true
                    && e["payload"]["images"].as_array().is_some_and(|l| !l.is_empty())
            })
        })
        .await;
    assert!(shown, "the full frame of the new socket has the placement");
    let list = last_images(&again, pane).expect("the list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].as_array().expect("array")[..2], placement[..2], "the same id and generation");
    let bytes = again.raw_call("image_data", &read_args(pane, &placement, 0)).await.expect("the pixels again");
    assert_eq!(bytes[16..], RED_GREEN);
    println!("reattach: placement again with id {} generation {}, pixels {}", placement[0], placement[1], hex(&bytes[16..]));
}

// ---- links --------------------------------------------------------------------------------------------------------------

/// Prints `command` at the top left of a cleared screen, waits for a marker on the row below and returns row 0 of the screen.
async fn print_row(line: &mut Line, pane: u64, n: usize, command: &str) -> Value {
    let marker = format!("hw-link-case-{n}");
    run_and_wait(line, pane, &format!("{CLEAR}; {command}; printf '\\n'"), &marker).await;
    rows_of(line, pane)[0].clone()
}

fn spans(row: &Value) -> Vec<(u64, u64, String)> {
    row["links"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|s| (s[0].as_u64().unwrap_or(u64::MAX), s[1].as_u64().unwrap_or(u64::MAX), s[2].as_str().unwrap_or_default().to_owned()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_osc_8_link_is_a_span_in_its_row_and_the_link_flag_in_the_runs_of_its_cells() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;

    // The control first: the same word, no sequence.
    let plain = print_row(&mut line, pane, 0, "printf 'docs'").await;
    assert!(plain.get("links").is_none(), "a row with no link has no links key: {plain}");
    assert!(cell_flags(&plain).iter().all(|f| f & 16 == 0), "no cell carries the link flag: {plain}");

    let row = print_row(&mut line, pane, 1, &osc8_command("https://example.org/a", "docs")).await;
    assert_eq!(spans(&row), [(0, 3, "https://example.org/a".to_owned())], "four cells, the address the program gave: {row}");
    let flags = cell_flags(&row);
    assert!(flags.len() >= 4, "{row}");
    assert!(flags[..4].iter().all(|f| f & 16 == 16), "the first four cells carry the flag (16): {flags:?}");
    assert!(flags.get(4).is_none_or(|f| f & 16 == 0), "the fifth cell does not: {flags:?}");
    println!("link: span [0, 3, \"https://example.org/a\"], flags of the first five cells {:?}", &flags[..flags.len().min(5)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_address_the_core_does_not_accept_is_no_span_and_an_accepted_one_beside_it_is() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;

    // (what, the command that prints the link, the address of the one span that is due, or None for no span). The address is
    // `None` to mean "any" for the cases whose length is the point.
    let long_ok = parity_env::osc8_command_of_length("https://example.org/", 2000, "docs");
    let long_bad = parity_env::osc8_command_of_length("https://example.org/", 2049, "docs");
    let some = |uri: &str| Some(uri.to_owned());
    let cases: Vec<(&str, String, Option<String>)> = vec![
        ("a plain https address (control)", osc8_command("https://example.org/a", "docs"), some("https://example.org/a")),
        ("an address of 2000 bytes (control for the length)", long_ok, Some(format!("https://example.org/{}", "a".repeat(1980)))),
        ("an address of 2049 bytes", long_bad, None),
        ("an address with a blank", osc8_command("https://example.org/a b", "docs"), None),
        // A control character inside the string is dropped by the terminal's parser before the core sees the address, so the
        // span holds the address without it: still printable ASCII only (the test says what is observed, not what was hoped).
        (
            "an address with a control character (0x01) inside",
            osc8_command("https://example.org/a\\001b", "docs"),
            some("https://example.org/ab"),
        ),
        ("an address with DEL (0x7f)", osc8_command("https://example.org/a\\177b", "docs"), None),
        ("an address with a non-ASCII letter", osc8_command("https://example.org/a\\303\\251b", "docs"), None),
        ("javascript:", osc8_command("javascript:alert(1)", "docs"), None),
        ("JaVaScRiPt: in mixed case", osc8_command("JaVaScRiPt:alert(1)", "docs"), None),
        ("data:", osc8_command("data:text/html,x", "docs"), None),
        ("vbscript:", osc8_command("vbscript:x", "docs"), None),
        ("an address with no scheme", osc8_command("example.org/a", "docs"), None),
        (
            "an address with markup (no blank)",
            osc8_command("https://example.org/<img/src=x/onerror=alert(1)>", "docs"),
            some("https://example.org/<img/src=x/onerror=alert(1)>"),
        ),
        // The scheme list is the host's business (the page and the opener decide), so the core keeps these as spans.
        ("ssh:// (the host's list decides, not the core)", osc8_command("ssh://host/x", "docs"), some("ssh://host/x")),
        ("file:// (the host's list decides, not the core)", osc8_command("file:///etc/hosts", "docs"), some("file:///etc/hosts")),
    ];
    let mut report = Vec::new();
    for (n, (what, command, due)) in cases.iter().enumerate() {
        let row = print_row(&mut line, pane, 100 + n, command).await;
        let found = spans(&row);
        if let Some(uri) = due {
            assert_eq!(found.len(), 1, "{what}: one span was due: {row}");
            assert_eq!((found[0].0, found[0].1), (0, 3), "{what}: the span covers the word: {found:?}");
            assert_eq!(&found[0].2, uri, "{what}: the address");
            assert!(found[0].2.bytes().all(|b| (0x21..=0x7e).contains(&b)), "{what}: the address is printable ASCII without a blank");
            assert!(cell_flags(&row)[..4].iter().all(|f| f & 16 == 16), "{what}: the flag is on the word");
        } else {
            assert!(found.is_empty(), "{what}: no span was due, got {found:?}");
            assert!(row.get("links").is_none(), "{what}: not even an empty links key: {row}");
            assert!(cell_flags(&row).iter().all(|f| f & 16 == 0), "{what}: no cell carries the flag: {row}");
        }
        report.push(format!(
            "{what}: {}",
            if found.is_empty() { "no span".to_owned() } else { format!("span of {} bytes", found[0].2.len()) }
        ));
    }
    println!("Hostile links: {}", report.join("; "));
}
