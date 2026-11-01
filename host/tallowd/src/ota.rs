//! OTA from the host, over the root's serial port.
//!
//! The node reads `O <hex id> <hex payload>`. The payload is what
//! `tallow_mesh_core::ota` encodes. Chunks go out at most 20 a second. The
//! host does not send another one inside that gap: the node already keeps one
//! frame in flight on each hop. The wait takes lines and a clock, as `gatt`
//! does, so the tests open no port.
//!
//! The order is: wait to hear the target, Begin until a Status says it landed,
//! the chunks, a Query per window with resends, then Commit until a Result
//! comes back. `<image>.sig` from `tools/mesh.sh ota-sign` is the product and
//! version as well as the signature, so this needs nothing else to fill Begin.

use crate::gatt::{self, Port, LINE_MAX, READY_S};
use crate::link::{parse_line, Link};
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::result::Result as StdResult;
use tallow_mesh_core::ota::{
    self, Code, Down, Up, CHUNK, PRODUCT_NODE, SLOT, WINDOW, WINDOW_BYTES,
};
use tallow_mesh_core::wire::{FrameKind, MAX_PAYLOAD};

/// How long the host waits between downward lines. Twenty a second.
const GAP_S: f64 = 0.050;
/// How long a Status may take.
const WAIT_S: f64 = 5.0;
/// Queries of one window before the transfer gives up.
const RESEND_ROUNDS: u32 = 8;
/// How long Begin gets to land.
const OPEN_S: f64 = 60.0;
/// A Query goes out this often while Begin lands.
const OPEN_POLL_S: f64 = 2.0;
/// Begin goes out again after this long with no answer at all.
const BEGIN_AGAIN_S: f64 = 10.0;
/// A Query that finds no transfer answers Refused, and so does a Begin the node
/// could not start. This many of them ends the run. Fewer would lose to one lost
/// Begin.
const OPEN_REFUSALS: u32 = 3;
/// How long Commit gets to answer. The node checks the whole image on flash.
const COMMIT_S: f64 = 30.0;
/// With no Result for this long, a Query asks whether Commit landed.
const COMMIT_QUERY_S: f64 = 10.0;

/// What the command line named.
#[derive(Debug, PartialEq, Eq)]
pub struct Job {
    pub port: String,
    pub target: u32,
    pub image: PathBuf,
    pub sig: PathBuf,
}

/// Bytes in `<image>.sig`: product, version, signature.
pub const SIG_FILE: usize = 69;

/// What `tools/mesh.sh ota-sign` wrote, and all that Begin needs from it.
#[derive(Debug, PartialEq, Eq)]
pub struct Signed {
    pub product: u8,
    pub version: u32,
    pub signature: [u8; 64],
}

impl Signed {
    /// Exactly [`SIG_FILE`] bytes: product, version little-endian, signature.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; SIG_FILE] = bytes.try_into().ok()?;
        let (&product, rest) = bytes.split_first()?;
        let (version, signature) = rest.split_first_chunk::<4>()?;
        Some(Self {
            product,
            version: u32::from_le_bytes(*version),
            signature: signature.try_into().ok()?,
        })
    }
}

/// A transfer that was sent and did not finish with Result ok.
#[derive(Debug, PartialEq, Eq)]
pub enum Fail {
    /// The line would not fit in the node's buffer, or the payload will not encode.
    Line,
    /// The target sent nothing up after the root's reset, so no route exists.
    NotHeard,
    Timeout,
    /// The root printed `@O refused`.
    Route,
    /// The image is empty or does not fit the slot.
    Image,
    /// The `.sig` names a product other than a mesh node.
    Product(u8),
    Result(Code),
    /// Commit went out and no Result was seen, so the outcome is not known.
    Unconfirmed,
}

impl std::fmt::Display for Fail {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Line => write!(
                out,
                "the line does not fit the node's {LINE_MAX} byte buffer"
            ),
            Fail::NotHeard => write!(
                out,
                "the target sent nothing up in {READY_S} s, so the root has no route to it"
            ),
            Fail::Timeout => write!(out, "timed out"),
            Fail::Route => write!(out, "the root refused the line"),
            Fail::Image => write!(out, "the image is empty or larger than the slot"),
            Fail::Product(product) => write!(
                out,
                "the image is signed for product {product}, and only a mesh node (product {PRODUCT_NODE}) takes it here"
            ),
            Fail::Result(Code::Ok) => write!(out, "the node said ok where it should not"),
            Fail::Result(Code::BadSignature) => write!(
                out,
                "the node refused the signature: the image, product, version, or key differs from what was signed"
            ),
            Fail::Result(Code::BadHash) => write!(
                out,
                "the bytes the node received do not hash to the signed digest"
            ),
            Fail::Result(Code::Missing) => {
                write!(out, "the node is still missing chunks at Commit")
            }
            Fail::Result(Code::Refused) => write!(
                out,
                "the node refused: no transfer is running, or it could not start or store one"
            ),
            Fail::Result(Code::Downgrade) => write!(
                out,
                "the node refused a downgrade: this version is below the highest it has confirmed"
            ),
            Fail::Result(Code::WrongProduct) => {
                write!(out, "the node refused an image signed for another product")
            }
            Fail::Unconfirmed => write!(
                out,
                "no Result was seen after Commit, so the boot partition may be set: read the node before sending again"
            ),
        }
    }
}

/// `tallowd ota --port <p> --target <id> --image <file> --sig <file>`. Opens the port.
pub fn command(argv: &[String]) -> Result<i32> {
    let job = parse_args(argv)?;
    let image = std::fs::read(&job.image)
        .with_context(|| format!("could not read {}", job.image.display()))?;
    let signed = read_signed(&job.sig)?;
    if let Some(fail) = refusal(&image, &signed) {
        eprintln!("tallowd ota: {fail}");
        return Ok(1);
    }
    let mut link = Link::open(&job.port)?;
    match transfer(&mut link, job.target, &image, &signed, &mut |line| {
        println!("{line}");
    })? {
        Ok(()) => {
            println!("stored: the node reboots into it, and rolls it back unless it proves itself");
            Ok(0)
        }
        Err(fail) => {
            eprintln!("tallowd ota: {fail}");
            Ok(1)
        }
    }
}

/// Runs before the port opens, because opening it resets the root.
fn read_signed(path: &std::path::Path) -> Result<Signed> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    match Signed::parse(&bytes) {
        Some(signed) => Ok(signed),
        None => bail!(
            "{} is {} bytes, not {SIG_FILE}. Sign the image with tools/mesh.sh ota-sign",
            path.display(),
            bytes.len()
        ),
    }
}

/// What the host refuses without asking the node. Runs before the port opens,
/// because opening it resets the root.
fn refusal(image: &[u8], signed: &Signed) -> Option<Fail> {
    if image.is_empty() || image.len() > SLOT as usize {
        return Some(Fail::Image);
    }
    (signed.product != PRODUCT_NODE).then_some(Fail::Product(signed.product))
}

/// Why a step stopped: a transfer that failed, or a port that did.
enum Stop {
    Fail(Fail),
    Port(anyhow::Error),
}

impl From<anyhow::Error> for Stop {
    fn from(error: anyhow::Error) -> Self {
        Self::Port(error)
    }
}

impl From<Fail> for Stop {
    fn from(fail: Fail) -> Self {
        Self::Fail(fail)
    }
}

/// Sends one image. `report` gets each stage, with the milliseconds since the
/// start. `Ok(Ok(()))` is Result ok. `Ok(Err)` is a finished transfer that was
/// not ok. A port error is the outer `Err`.
pub fn transfer<P: Port>(
    port: &mut P,
    target: u32,
    image: &[u8],
    signed: &Signed,
    report: &mut dyn FnMut(&str),
) -> Result<StdResult<(), Fail>> {
    if image.is_empty() || image.len() > SLOT as usize {
        return Ok(Err(Fail::Image));
    }
    if !gatt::wait_for_target(port, target, READY_S)? {
        return Ok(Err(Fail::NotHeard));
    }
    let start = port.clock();
    let mut sender = Sender {
        port,
        target,
        start,
        not_before: start,
        inbox: VecDeque::new(),
        held: false,
        report,
    };
    match sender.run(image, signed) {
        Ok(()) => Ok(Ok(())),
        Err(Stop::Fail(fail)) => Ok(Err(fail)),
        Err(Stop::Port(error)) => Err(error),
    }
}

/// One transfer. `inbox` keeps what a read brought in and no step has used yet,
/// because one read can return more than one line.
struct Sender<'a, P> {
    port: &'a mut P,
    target: u32,
    start: f64,
    not_before: f64,
    inbox: VecDeque<Up>,
    /// While Begin lands and after Commit, a failed Result goes to the step that
    /// is waiting, which knows what it means. Otherwise it ends the transfer.
    held: bool,
    report: &'a mut dyn FnMut(&str),
}

impl<P: Port> Sender<'_, P> {
    fn run(&mut self, image: &[u8], signed: &Signed) -> StdResult<(), Stop> {
        self.open(&Down::Begin {
            size: image.len() as u32,
            product: signed.product,
            version: signed.version,
            digest: digest_of(image),
            signature: signed.signature,
        })?;
        let mut offset = 0u32;
        while (offset as usize) < image.len() {
            self.send(&chunk_at(image, offset), "chunk")?;
            offset += CHUNK as u32;
        }
        let count = image.len().div_ceil(CHUNK);
        let windows = u16::try_from(count.div_ceil(WINDOW)).unwrap_or(u16::MAX);
        for window in 0..windows {
            self.settle(window, image)?;
        }
        self.commit()
    }

    /// Sends Begin and polls with Query until a Status says it landed. A node
    /// that answers with a refusal of its own ends the run with that code.
    fn open(&mut self, begin: &Down) -> StdResult<(), Stop> {
        self.held = true;
        let deadline = self.port.clock() + OPEN_S;
        self.send(begin, "begin")?;
        let mut last_begin = self.port.clock();
        let mut refusals = 0;
        loop {
            // A node that keeps answering Result ok would otherwise never reach the None arm.
            if self.port.clock() >= deadline {
                self.stage("timeout");
                return Err(Fail::Timeout.into());
            }
            self.send(&Down::Query { window: 0 }, "query")?;
            match self.reply(OPEN_POLL_S)? {
                Some(Up::Status { .. }) => {
                    self.held = false;
                    return Ok(());
                }
                Some(Up::Result {
                    code: Code::Refused,
                }) => {
                    refusals += 1;
                    if refusals >= OPEN_REFUSALS {
                        return Err(Fail::Result(Code::Refused).into());
                    }
                    self.send(begin, "begin")?;
                    last_begin = self.port.clock();
                }
                Some(Up::Result { code: Code::Ok }) => {}
                Some(Up::Result { code }) => return Err(Fail::Result(code).into()),
                None => {
                    let now = self.port.clock();
                    if now >= deadline {
                        self.stage("timeout");
                        return Err(Fail::Timeout.into());
                    }
                    if now - last_begin >= BEGIN_AGAIN_S {
                        self.send(begin, "begin")?;
                        last_begin = self.port.clock();
                    }
                }
            }
        }
    }

    /// Queries one window and resends what it says is missing, until none is.
    fn settle(&mut self, window: u16, image: &[u8]) -> StdResult<(), Stop> {
        for _ in 0..RESEND_ROUNDS {
            self.inbox.clear();
            self.send(&Down::Query { window }, "query")?;
            let Some(Up::Status {
                window: got,
                missing,
            }) = self.reply(WAIT_S)?
            else {
                continue;
            };
            if got != window {
                continue;
            }
            let holes = missing_offsets(window, &missing, image.len());
            self.stage(&format!("status {window} missing {}", holes.len()));
            if holes.is_empty() {
                return Ok(());
            }
            for hole in holes {
                self.send(&chunk_at(image, hole), "resend")?;
            }
        }
        self.stage("timeout");
        Err(Fail::Timeout.into())
    }

    /// Sends Commit and waits for its Result. With no Result, a Query tells the
    /// cases apart: a Status means Commit never landed, so it goes again. A
    /// Refused means the transfer is over and its Result was lost, and Refused
    /// is also what a failed store answers, so neither can be called a success
    /// or a failure.
    fn commit(&mut self) -> StdResult<(), Stop> {
        self.held = true;
        let deadline = self.port.clock() + COMMIT_S;
        self.send(&Down::Commit, "commit")?;
        let mut asked = false;
        loop {
            let left = deadline - self.port.clock();
            match self.reply(left.min(COMMIT_QUERY_S))? {
                Some(Up::Result { code: Code::Ok }) => {
                    self.stage("result ok");
                    return Ok(());
                }
                Some(Up::Result {
                    code: Code::Refused,
                }) if asked => return Err(Fail::Unconfirmed.into()),
                Some(Up::Result { code }) => return Err(Fail::Result(code).into()),
                Some(Up::Status { .. }) if asked => {
                    asked = false;
                    self.send(&Down::Commit, "commit")?;
                }
                Some(Up::Status { .. }) => {}
                None if self.port.clock() >= deadline => {
                    self.stage("timeout");
                    return Err(Fail::Unconfirmed.into());
                }
                None => {
                    asked = true;
                    self.send(&Down::Query { window: 0 }, "query")?;
                }
            }
        }
    }

    /// Sends one downward line, after the gap.
    fn send(&mut self, message: &Down, label: &str) -> StdResult<(), Stop> {
        while self.port.clock() < self.not_before {
            for line in self.port.read()? {
                self.sort(&line)?;
            }
        }
        let line = down_line(self.target, message).ok_or(Fail::Line)?;
        self.port.send(&line)?;
        let detail = stage_detail(message);
        if detail.is_empty() {
            self.stage(label);
        } else {
            self.stage(&format!("{label} {detail}"));
        }
        self.not_before = self.port.clock() + GAP_S;
        Ok(())
    }

    /// The next message up, or `None` when `seconds` pass without one.
    fn reply(&mut self, seconds: f64) -> StdResult<Option<Up>, Stop> {
        let deadline = self.port.clock() + seconds;
        loop {
            if let Some(up) = self.inbox.pop_front() {
                self.stage("up");
                return Ok(Some(up));
            }
            if self.port.clock() >= deadline {
                return Ok(None);
            }
            for line in self.port.read()? {
                self.sort(&line)?;
            }
        }
    }

    /// Keeps what one line means for the target, and stops on a refusal.
    fn sort(&mut self, line: &str) -> StdResult<(), Stop> {
        if line.trim() == "@O refused" {
            self.stage("refused");
            return Err(Fail::Route.into());
        }
        let Some(up) = up_from(line, self.target) else {
            return Ok(());
        };
        if let Up::Result { code } = up {
            if code != Code::Ok && !self.held {
                self.stage(&format!("result {code:?}"));
                return Err(Fail::Result(code).into());
            }
        }
        self.inbox.push_back(up);
        Ok(())
    }

    fn stage(&mut self, text: &str) {
        let ms = elapsed_ms(self.start, self.port.clock());
        (self.report)(&format!("{text} {ms} ms"));
    }
}

fn digest_of(image: &[u8]) -> [u8; ota::DIGEST] {
    let mut hash = ota::ImageHash::new();
    hash.update(image);
    hash.finish()
}

fn chunk_at(image: &[u8], offset: u32) -> Down {
    let start = offset as usize;
    let end = (start + CHUNK).min(image.len());
    let bytes = image.get(start..end).unwrap_or(&[]);
    let mut data = [0u8; CHUNK];
    data[..bytes.len()].copy_from_slice(bytes);
    Down::Chunk {
        offset,
        len: bytes.len() as u8,
        data,
    }
}

fn missing_offsets(window: u16, missing: &[u8; WINDOW_BYTES], image_len: usize) -> Vec<u32> {
    let mut holes = Vec::new();
    for bit in 0..WINDOW {
        let set = missing
            .get(bit / 8)
            .is_some_and(|byte| byte & (1 << (bit % 8)) != 0);
        if !set {
            continue;
        }
        let index = usize::from(window) * WINDOW + bit;
        let offset = index * CHUNK;
        if offset < image_len {
            holes.push(offset as u32);
        }
    }
    holes
}

fn stage_detail(message: &Down) -> String {
    match *message {
        Down::Begin {
            size,
            product,
            version,
            ..
        } => format!("size {size} product {product} version {version}"),
        Down::Chunk { offset, .. } => format!("offset {offset}"),
        Down::Query { window } => format!("window {window}"),
        Down::Commit => String::new(),
    }
}

fn up_from(line: &str, target: u32) -> Option<Up> {
    let parsed = parse_line(line)?;
    if parsed.frame != u32::from(FrameKind::OtaUp as u8) || !parsed.is_from(target) {
        return None;
    }
    ota::decode_up(&parsed.payload)
}

/// The `O` line for one payload, or `None` when it will not encode or does not fit.
pub fn down_line(target: u32, message: &Down) -> Option<String> {
    let mut body = [0u8; MAX_PAYLOAD - 4];
    let length = ota::encode_down(message, &mut body)?;
    let hex: String = body[..length]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let line = format!("O {target:08x} {hex}\n");
    gatt::fits(&line).then_some(line)
}

fn elapsed_ms(start: f64, now: f64) -> u64 {
    let ms = (now - start) * 1000.0;
    if !ms.is_finite() || ms < 0.0 {
        0
    } else {
        ms as u64
    }
}

pub fn parse_args(argv: &[String]) -> Result<Job> {
    let mut port = None;
    let mut target = None;
    let mut image = None;
    let mut sig = None;
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--port" => port = Some(words.next().context("--port needs a value")?.clone()),
            "--target" => {
                let text = words.next().context("--target needs a node id")?;
                target = Some(node_id(text)?);
            }
            "--image" => image = Some(PathBuf::from(words.next().context("--image needs a file")?)),
            "--sig" => sig = Some(PathBuf::from(words.next().context("--sig needs a file")?)),
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(Job {
        port: port.context("--port is required")?,
        target: target.context("--target is required")?,
        image: image.context("--image is required")?,
        sig: sig.context("--sig is required")?,
    })
}

fn node_id(text: &str) -> Result<u32> {
    let hex = text.strip_prefix("0x").unwrap_or(text);
    u32::from_str_radix(hex, 16).with_context(|| format!("{text} is not a node id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_compact::{KeyPair, Seed};
    use tallow_mesh_core::ota::{signed_message, verify, Action, Receiver, PRODUCT_NODE};

    const TARGET: u32 = 0x0a0b_0c0d;
    const VERSION: u32 = 3;

    /// What goes wrong on the way to the node.
    #[derive(Default)]
    struct Trouble {
        /// The first chunk at this offset is not delivered. A later one is.
        drop_chunk: Option<u32>,
        /// This chunk arrives with a byte flipped.
        corrupt_chunk: Option<u32>,
        drop_begins: u32,
        drop_commits: u32,
        /// Commit lands, and its ok Result does not come back.
        lose_result: bool,
        /// Nothing reaches the node before this.
        mute_until: f64,
        /// The flash refuses Begin, as with a slot pending verify.
        flash_refuses: bool,
        /// A line from another node that looks like the answer to everything.
        other_node_refuses: bool,
    }

    /// A root just after its reset, the node behind it, and a lossy link. Until
    /// the target has spoken, the root refuses as `send_down` does.
    struct Lossy {
        now: f64,
        heard_at: f64,
        routed: bool,
        receiver: Receiver,
        public: [u8; 32],
        trouble: Trouble,
        dropped: bool,
        begins: u32,
        commits: u32,
        actions: Vec<Action>,
        pending: Vec<String>,
        sent_at: Vec<f64>,
    }

    impl Port for Lossy {
        fn clock(&self) -> f64 {
            self.now
        }

        fn read(&mut self) -> Result<Vec<String>> {
            self.now += GAP_S;
            if self.now >= self.heard_at && !self.routed {
                self.routed = true;
                return Ok(vec![format!("@U 2 {TARGET:08x} 1 1 1 -40")]);
            }
            Ok(std::mem::take(&mut self.pending))
        }

        fn send(&mut self, line: &str) -> Result<()> {
            self.sent_at.push(self.now);
            if !self.routed {
                self.pending.push("@O refused".into());
                return Ok(());
            }
            if self.now < self.trouble.mute_until {
                return Ok(());
            }
            let mut body = hex_body(line);
            match ota::decode_down(&body) {
                Some(Down::Begin { .. }) => {
                    self.begins += 1;
                    if self.trouble.drop_begins > 0 {
                        self.trouble.drop_begins -= 1;
                        return Ok(());
                    }
                }
                Some(Down::Commit) => {
                    self.commits += 1;
                    if self.trouble.drop_commits > 0 {
                        self.trouble.drop_commits -= 1;
                        return Ok(());
                    }
                }
                Some(Down::Chunk { offset, .. }) => {
                    if self.trouble.drop_chunk == Some(offset) && !self.dropped {
                        self.dropped = true;
                        return Ok(());
                    }
                    if self.trouble.corrupt_chunk == Some(offset) {
                        // Past the sub-kind, offset, and length.
                        let last = body.len() - 1;
                        body[last] ^= 0xff;
                    }
                }
                _ => {}
            }
            if self.trouble.other_node_refuses {
                self.pending.push(up_line(
                    &Up::Result {
                        code: Code::Missing,
                    },
                    5,
                ));
            }
            let up = self.receiver.push(&body, &self.public);
            while let Some(action) = self.receiver.take_action() {
                if self.trouble.flash_refuses && matches!(action, Action::Begin { .. }) {
                    self.pending.push(up_line(
                        &Up::Result {
                            code: Code::Refused,
                        },
                        TARGET,
                    ));
                    self.receiver = Receiver::new(PRODUCT_NODE, 0);
                    return Ok(());
                }
                self.actions.push(action);
            }
            if let Some(up) = up {
                if up == (Up::Result { code: Code::Ok }) && self.trouble.lose_result {
                    return Ok(());
                }
                self.pending.push(up_line(&up, TARGET));
            }
            Ok(())
        }

        fn next_id(&mut self) -> u16 {
            1
        }
    }

    fn hex_body(line: &str) -> Vec<u8> {
        let hex = line.split_whitespace().nth(2).expect("hex");
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect()
    }

    fn up_line(message: &Up, origin: u32) -> String {
        let mut out = [0u8; MAX_PAYLOAD];
        let length = ota::encode_up(message, &mut out).expect("up");
        let hex: String = out[..length]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("@U 13 {origin:08x} 1 1 1 -40 {hex}")
    }

    fn signed(image: &[u8]) -> (Signed, [u8; 32]) {
        let pair = KeyPair::from_seed(Seed::new([0x42; 32]));
        let message = signed_message(PRODUCT_NODE, VERSION, &digest_of(image));
        let signature = *pair.sk.sign(message, None);
        (
            Signed {
                product: PRODUCT_NODE,
                version: VERSION,
                signature,
            },
            *pair.pk,
        )
    }

    fn image_of(size: usize) -> Vec<u8> {
        (0..size)
            .map(|index| (index.wrapping_mul(3) + 1) as u8)
            .collect()
    }

    fn port(public: [u8; 32], floor: u32, trouble: Trouble) -> Lossy {
        Lossy {
            now: 0.0,
            heard_at: 3.0,
            routed: false,
            receiver: Receiver::new(PRODUCT_NODE, floor),
            public,
            trouble,
            dropped: false,
            begins: 0,
            commits: 0,
            actions: Vec::new(),
            pending: Vec::new(),
            sent_at: Vec::new(),
        }
    }

    fn run(image: &[u8], signed: &Signed, port: &mut Lossy) -> StdResult<(), Fail> {
        transfer(port, TARGET, image, signed, &mut |_| {}).expect("port")
    }

    fn finished(port: &Lossy) -> bool {
        port.actions.contains(&Action::Finish { version: VERSION })
    }

    #[test]
    fn a_lossy_image_finishes_and_a_bad_signature_does_not() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                drop_chunk: Some(170),
                ..Trouble::default()
            },
        );
        assert_eq!(run(&image, &good, &mut lossy), Ok(()));
        assert!(lossy.dropped);
        assert!(finished(&lossy));
        let mut previous = None;
        for at in &lossy.sent_at {
            if let Some(before) = previous {
                assert!(*at - before >= GAP_S - f64::EPSILON);
            }
            previous = Some(*at);
        }
        assert!(lossy.sent_at.len() >= 2);

        let mut bad = signed(&image).0;
        bad.signature[0] ^= 0xff;
        let mut refused = port(public, 0, Trouble::default());
        assert_eq!(
            run(&image, &bad, &mut refused),
            Err(Fail::Result(Code::BadSignature))
        );
        assert!(refused.actions.is_empty());
        assert_eq!(refused.commits, 0);
    }

    #[test]
    fn a_transfer_waits_for_the_target_before_begin() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(public, 0, Trouble::default());
        assert_eq!(run(&image, &good, &mut lossy), Ok(()));
        assert!(lossy.sent_at.first().is_some_and(|at| *at >= 3.0));
    }

    #[test]
    fn a_target_that_never_speaks_fails_without_sending() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(public, 0, Trouble::default());
        lossy.heard_at = f64::INFINITY;
        assert_eq!(run(&image, &good, &mut lossy), Err(Fail::NotHeard));
        assert!(lossy.now >= READY_S);
        assert!(lossy.sent_at.is_empty());
    }

    #[test]
    fn a_lost_begin_is_sent_again() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                drop_begins: 1,
                ..Trouble::default()
            },
        );
        assert_eq!(run(&image, &good, &mut lossy), Ok(()));
        assert_eq!(lossy.begins, 2);
        assert!(finished(&lossy));
    }

    #[test]
    fn a_node_that_does_not_answer_gets_begin_again() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                mute_until: 3.0 + BEGIN_AGAIN_S,
                ..Trouble::default()
            },
        );
        assert_eq!(run(&image, &good, &mut lossy), Ok(()));
        assert_eq!(lossy.begins, 1);
        assert!(finished(&lossy));
    }

    #[test]
    fn a_node_that_stays_silent_times_out_inside_the_cap() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                mute_until: f64::INFINITY,
                ..Trouble::default()
            },
        );
        assert_eq!(run(&image, &good, &mut lossy), Err(Fail::Timeout));
        assert!(lossy.now <= 3.0 + OPEN_S + 2.0 * OPEN_POLL_S);
    }

    #[test]
    fn a_flash_that_refuses_begin_ends_the_run_with_refused() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                flash_refuses: true,
                ..Trouble::default()
            },
        );
        assert_eq!(
            run(&image, &good, &mut lossy),
            Err(Fail::Result(Code::Refused))
        );
        assert!(lossy.begins <= OPEN_REFUSALS);
        assert_eq!(lossy.commits, 0);
    }

    #[test]
    fn a_downgrade_is_refused_at_once() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(public, VERSION + 1, Trouble::default());
        assert_eq!(
            run(&image, &good, &mut lossy),
            Err(Fail::Result(Code::Downgrade))
        );
        assert_eq!(lossy.begins, 1);
        assert!(lossy.actions.is_empty());
    }

    #[test]
    fn an_image_for_another_product_is_refused_at_once() {
        let image = image_of(400);
        let (mut good, public) = signed(&image);
        good.product = PRODUCT_NODE + 1;
        let mut lossy = port(public, 0, Trouble::default());
        assert_eq!(
            run(&image, &good, &mut lossy),
            Err(Fail::Result(Code::WrongProduct))
        );
        assert_eq!(lossy.begins, 1);
    }

    #[test]
    fn each_refusal_says_something_different() {
        let codes = [
            Code::BadSignature,
            Code::BadHash,
            Code::Missing,
            Code::Refused,
            Code::Downgrade,
            Code::WrongProduct,
        ];
        let mut texts: Vec<String> = codes
            .iter()
            .map(|code| Fail::Result(*code).to_string())
            .collect();
        assert!(texts[0].contains("signature"));
        assert!(texts[4].contains("downgrade"));
        assert!(texts[5].contains("another product"));
        texts.sort();
        texts.dedup();
        assert_eq!(texts.len(), codes.len());
    }

    #[test]
    fn another_nodes_refusal_is_not_this_transfers() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                other_node_refuses: true,
                ..Trouble::default()
            },
        );
        assert_eq!(run(&image, &good, &mut lossy), Ok(()));
        assert!(finished(&lossy));
    }

    #[test]
    fn a_refusal_in_the_middle_is_reported_as_itself() {
        let image = image_of(CHUNK * 4);
        let (good, public) = signed(&image);
        let mut lossy = port(public, 0, Trouble::default());
        let mut sends = 0;
        let mut seen = Vec::new();
        // The node reboots after the third chunk, so the next one finds no transfer.
        struct Reboots<'a> {
            inner: &'a mut Lossy,
            sends: &'a mut u32,
        }
        impl Port for Reboots<'_> {
            fn clock(&self) -> f64 {
                self.inner.clock()
            }
            fn read(&mut self) -> Result<Vec<String>> {
                self.inner.read()
            }
            fn send(&mut self, line: &str) -> Result<()> {
                if matches!(ota::decode_down(&hex_body(line)), Some(Down::Chunk { .. })) {
                    *self.sends += 1;
                    if *self.sends == 3 {
                        self.inner.receiver = Receiver::new(PRODUCT_NODE, 0);
                    }
                }
                self.inner.send(line)
            }
            fn next_id(&mut self) -> u16 {
                1
            }
        }
        let mut wrapped = Reboots {
            inner: &mut lossy,
            sends: &mut sends,
        };
        let result = transfer(&mut wrapped, TARGET, &image, &good, &mut |line| {
            seen.push(line.to_string());
        })
        .expect("port");
        assert_eq!(result, Err(Fail::Result(Code::Refused)));
        assert!(seen.iter().any(|line| line.starts_with("result Refused")));
        assert_eq!(lossy.commits, 0);
    }

    #[test]
    fn a_commit_that_finds_bad_bytes_reports_the_hash() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                corrupt_chunk: Some(170),
                ..Trouble::default()
            },
        );
        assert_eq!(
            run(&image, &good, &mut lossy),
            Err(Fail::Result(Code::BadHash))
        );
        assert!(!finished(&lossy));
    }

    #[test]
    fn a_lost_commit_is_found_by_a_query_and_sent_again() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                drop_commits: 1,
                ..Trouble::default()
            },
        );
        assert_eq!(run(&image, &good, &mut lossy), Ok(()));
        assert_eq!(lossy.commits, 2);
        assert!(finished(&lossy));
    }

    #[test]
    fn a_lost_result_is_unconfirmed_and_never_a_pass() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(
            public,
            0,
            Trouble {
                lose_result: true,
                ..Trouble::default()
            },
        );
        assert_eq!(run(&image, &good, &mut lossy), Err(Fail::Unconfirmed));
        assert!(finished(&lossy));
        assert!(Fail::Unconfirmed
            .to_string()
            .contains("boot partition may be set"));
    }

    #[test]
    fn a_node_gone_after_commit_is_unconfirmed() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(public, 0, Trouble::default());
        // Everything sent from the Commit on goes nowhere, which leaves the
        // transfer open on the node.
        struct Gone<'a>(&'a mut Lossy);
        impl Port for Gone<'_> {
            fn clock(&self) -> f64 {
                self.0.clock()
            }
            fn read(&mut self) -> Result<Vec<String>> {
                self.0.read()
            }
            fn send(&mut self, line: &str) -> Result<()> {
                if matches!(ota::decode_down(&hex_body(line)), Some(Down::Commit)) {
                    self.0.trouble.mute_until = f64::INFINITY;
                }
                self.0.send(line)
            }
            fn next_id(&mut self) -> u16 {
                1
            }
        }
        let result =
            transfer(&mut Gone(&mut lossy), TARGET, &image, &good, &mut |_| {}).expect("port");
        assert_eq!(result, Err(Fail::Unconfirmed));
        assert!(!finished(&lossy));
    }

    #[test]
    fn a_status_after_commit_with_no_question_asked_is_stale() {
        let image = image_of(400);
        let (good, public) = signed(&image);
        let mut lossy = port(public, 0, Trouble::default());
        // One extra Status arrives with the Result, as a late answer would.
        struct Late<'a>(&'a mut Lossy);
        impl Port for Late<'_> {
            fn clock(&self) -> f64 {
                self.0.clock()
            }
            fn read(&mut self) -> Result<Vec<String>> {
                self.0.read()
            }
            fn send(&mut self, line: &str) -> Result<()> {
                if matches!(ota::decode_down(&hex_body(line)), Some(Down::Commit)) {
                    self.0.pending.push(up_line(
                        &Up::Status {
                            window: 0,
                            missing: [0; WINDOW_BYTES],
                        },
                        TARGET,
                    ));
                }
                self.0.send(line)
            }
            fn next_id(&mut self) -> u16 {
                1
            }
        }
        let result =
            transfer(&mut Late(&mut lossy), TARGET, &image, &good, &mut |_| {}).expect("port");
        assert_eq!(result, Ok(()));
        assert_eq!(lossy.commits, 1);
    }

    #[test]
    fn a_chunk_line_for_the_largest_body_fits() {
        let image = vec![0x11; CHUNK];
        let line = down_line(0xffff_ffff, &chunk_at(&image, 0)).expect("line");
        let text = line.trim_end_matches('\n');
        assert_eq!(text.len(), 361);
        assert!(text.len() <= LINE_MAX);
        assert!(text.starts_with('O'));
    }

    #[test]
    fn a_begin_line_fits_and_carries_the_signed_fields() {
        let image = image_of(400);
        let (good, _) = signed(&image);
        let begin = Down::Begin {
            size: 400,
            product: good.product,
            version: good.version,
            digest: digest_of(&image),
            signature: good.signature,
        };
        let line = down_line(TARGET, &begin).expect("line");
        assert_eq!(ota::decode_down(&hex_body(&line)), Some(begin));
    }

    #[test]
    fn a_sig_file_is_exactly_product_version_signature() {
        let mut bytes = vec![2u8];
        bytes.extend_from_slice(&0x0102_0304u32.to_le_bytes());
        bytes.extend_from_slice(&[0x5a; 64]);
        assert_eq!(bytes.len(), SIG_FILE);
        assert_eq!(
            Signed::parse(&bytes),
            Some(Signed {
                product: 2,
                version: 0x0102_0304,
                signature: [0x5a; 64],
            })
        );
        assert_eq!(Signed::parse(&bytes[..64]), None);
        assert_eq!(Signed::parse(&bytes[..68]), None);
        bytes.push(0);
        assert_eq!(Signed::parse(&bytes), None);
    }

    #[test]
    fn a_sig_file_of_the_old_length_is_refused_with_a_reason() {
        let dir = temp_dir("sig");
        let _cleanup = TempDir(dir.clone());
        let path = dir.join("old.sig");
        std::fs::write(&path, [0u8; 64]).expect("sig");
        let error = read_signed(&path).expect_err("refused");
        assert!(error.to_string().contains("64 bytes, not 69"), "{error}");
    }

    #[test]
    fn the_host_refuses_a_bad_image_or_product_on_its_own() {
        let image = image_of(400);
        let (good, _) = signed(&image);
        assert_eq!(refusal(&image, &good), None);
        assert_eq!(refusal(&[], &good), Some(Fail::Image));
        assert_eq!(
            refusal(&vec![0; SLOT as usize + 1], &good),
            Some(Fail::Image)
        );
        let cam = Signed {
            product: PRODUCT_NODE + 1,
            ..good
        };
        assert_eq!(refusal(&image, &cam), Some(Fail::Product(PRODUCT_NODE + 1)));
    }

    /// The port named here is no serial port, so reaching `Link::open` is an error.
    #[test]
    fn a_refused_image_never_opens_the_port() {
        let dir = temp_dir("refuse");
        let _cleanup = TempDir(dir.clone());
        let image = image_of(400);
        let (good, _) = signed(&image);
        let mut sig = vec![PRODUCT_NODE + 1];
        sig.extend_from_slice(&good.version.to_le_bytes());
        sig.extend_from_slice(&good.signature);
        std::fs::write(dir.join("cam.bin"), &image).expect("image");
        std::fs::write(dir.join("cam.bin.sig"), &sig).expect("sig");
        std::fs::write(dir.join("empty.bin"), []).expect("image");
        sig[0] = PRODUCT_NODE;
        std::fs::write(dir.join("empty.bin.sig"), &sig).expect("sig");
        for name in ["cam.bin", "empty.bin"] {
            let argv: Vec<String> = [
                "--port",
                &dir.join("no-port").to_string_lossy(),
                "--target",
                "a0b0c0d",
                "--image",
                &dir.join(name).to_string_lossy(),
                "--sig",
                &dir.join(format!("{name}.sig")).to_string_lossy(),
            ]
            .iter()
            .map(|word| word.to_string())
            .collect();
            assert_eq!(command(&argv).expect(name), 1, "{name}");
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tallow-ota-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp");
        dir
    }

    /// The steps in `sign_ota_image` in `tools/mesh.sh`, with a throwaway key:
    /// plain Ed25519 over the 50-byte message, through `pkeyutl -rawin`.
    #[test]
    fn an_openssl_signature_over_the_signed_message_verifies() {
        let dir = temp_dir("openssl");
        let _cleanup = TempDir(dir.clone());
        let digest = digest_of(b"tallow ota openssl");
        let message_path = dir.join("message");
        std::fs::write(
            &message_path,
            signed_message(PRODUCT_NODE, VERSION, &digest),
        )
        .expect("message");
        let seed = dir.join("seed.der");
        let signature = dir.join("sig");
        let public = dir.join("pub.der");
        assert!(openssl(&[
            "genpkey",
            "-algorithm",
            "ED25519",
            "-outform",
            "DER",
            "-out",
            seed.to_str().expect("utf8"),
        ]));
        assert!(openssl(&[
            "pkey",
            "-inform",
            "DER",
            "-in",
            seed.to_str().expect("utf8"),
            "-pubout",
            "-outform",
            "DER",
            "-out",
            public.to_str().expect("utf8"),
        ]));
        assert!(openssl(&[
            "pkeyutl",
            "-sign",
            "-rawin",
            "-inkey",
            seed.to_str().expect("utf8"),
            "-keyform",
            "DER",
            "-in",
            message_path.to_str().expect("utf8"),
            "-out",
            signature.to_str().expect("utf8"),
        ]));
        let public_bytes = std::fs::read(&public).expect("pub");
        let signature_bytes = std::fs::read(&signature).expect("sig");
        let public: [u8; 32] = public_bytes[public_bytes.len() - 32..]
            .try_into()
            .expect("32");
        let signature: [u8; 64] = signature_bytes.as_slice().try_into().expect("64");
        assert!(verify(&public, PRODUCT_NODE, VERSION, &digest, &signature));
        assert!(!verify(
            &public,
            PRODUCT_NODE,
            VERSION + 1,
            &digest,
            &signature
        ));
        assert!(!verify(
            &public,
            PRODUCT_NODE + 1,
            VERSION,
            &digest,
            &signature
        ));
        let mut flipped = signature;
        flipped[0] ^= 0xff;
        assert!(!verify(&public, PRODUCT_NODE, VERSION, &digest, &flipped));
    }

    fn openssl(args: &[&str]) -> bool {
        std::process::Command::new("openssl")
            .args(args)
            .status()
            .is_ok_and(|status| status.success())
    }

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
