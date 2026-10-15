// Specification tests for the library `machine` (crate coremap-machine), public API only.
//
// Expected values come from the behaviour this library replaces, never from its code:
// - the C# sources: src\Profile.cs (Startup, Save, Read/Write, ProfileSets, FailureEvents), src\Startup.cs
//   (LimitsStartup.Named), src\LimitHold.cs (Recover), src\Tuning.cs (OcMailbox, CoreLimits), src\Topology.cs and
//   src\Block.cs (blocks), src\Json.cs (the writer's format);
// - outputs\resident-inventory-2026-10-14.md, sections 1.4-1.6 and 4;
// - the deliberate differences of DECISIONS.md #31: (a) trial hours end at a 6006 or a Kernel-Power 42 while failures
//   are still attributed by 6006 alone; (b) SMT siblings form one block; (c) after a write only the written block is
//   read back; (d) no mailbox request without its gate mutex and none over a busy mailbox; (e) «заводские» deletes
//   profile.json and its .bak;
// - the owner's rule of 15.10 on allowed ceilings (DECISIONS #31, the same in Tuning.cs CoreLimits since then): from
//   the fused maximum down to x8, the CPU's lowest efficient multiplier (LFM, 0xCE bits 47:40); nothing above fused
//   («Плюс 4 не надо… Никаких плюсов»). A ceiling above fused is refused before the mailbox;
// - facts measured on the owner's Core Ultra 9 275HX (KNOWLEDGE.md: answers to command 0x1C) and on this machine's
//   .NET Framework 4.8, the runtime of the C# writer (number and date formats).
// The comment at each test names the spec line it rests on. Nothing here touches the CPU: the processor is `SimCpu`
// or a fake defined below, and the fake register makes no driver call.
#![expect(
    unsafe_code,
    reason = "GetCurrentProcessorNumber: the fake register checks on which processor a request is written"
)]

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use machine::clock::{Pattern, Stamp};
use machine::events::{Event, EventLog};
use machine::files;
use machine::limits::Limits;
use machine::mailbox::{self, Allowed, Cpu, Limit, MailboxCpu};
use machine::marker::{self, Recovery};
use machine::named;
use machine::pawnio::Msr;
use machine::profile::{self, Profile, Set, Startup, Store};
use machine::sim::SimCpu;
use machine::topology::{Kind, Topology};
use machine::{HwError, Lp};

// ---------------------------------------------------------------------------------------------------------------
// The owner's machine
// ---------------------------------------------------------------------------------------------------------------

const CPU: &str = "Intel(R) Core(TM) Ultra 9 275HX";
const OTHER_CPU: &str = "Intel(R) Core(TM) Ultra 7 255HX";
const HOUR: i64 = 36_000_000_000; // in 100-ns ticks
// Windows logical processors of the 275HX: 0-1 P, 2-5 and 6-9 two E clusters, 10-13 P, 14-17 and 18-21 two E
// clusters, 22-23 P (no SMT).
const P_LPS: [usize; 8] = [0, 1, 10, 11, 12, 13, 22, 23];
const CLUSTERS: [[usize; 4]; 4] = [[2, 3, 4, 5], [6, 7, 8, 9], [14, 15, 16, 17], [18, 19, 20, 21]];
// Block.cs: an E cluster is named «first–last» with an en dash, any other core by its LP number.
const HX_BLOCKS: [(&str, &[usize]); 12] = [
    ("0", &[0]),
    ("1", &[1]),
    ("2\u{2013}5", &[2, 3, 4, 5]),
    ("6\u{2013}9", &[6, 7, 8, 9]),
    ("10", &[10]),
    ("11", &[11]),
    ("12", &[12]),
    ("13", &[13]),
    ("14\u{2013}17", &[14, 15, 16, 17]),
    ("18\u{2013}21", &[18, 19, 20, 21]),
    ("22", &[22]),
    ("23", &[23]),
];
// The owner's profile «csmart2» (data\profile.json): every ceiling at or below factory.
const SAFE: [i32; 24] = [
    47, 47, 45, 45, 45, 45, 36, 36, 36, 36, 48, 39, 47, 39, 46, 46, 46, 46, 32, 32, 32, 32, 48, 48,
];
const FIXTURE_TIME: &str = "2026-10-14T18:33:26.1031323-04:00";
const NO_WRITES: [(usize, u8); 0] = [];
const NO_SETS: [Set; 0] = [];
// data\profile.json and data\profiles.json exactly as the C# part wrote them on the owner's machine (copied text).
const PROFILE_FIXTURE: &str = r#"{"cpu": "Intel(R) Core(TM) Ultra 9 275HX", "name": "csmart2", "lps": [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23], "ratios": [47, 47, 45, 45, 45, 45, 36, 36, 36, 36, 48, 39, 47, 39, 46, 46, 46, 46, 32, 32, 32, 32, 48, 48], "state": "trusted", "saved": "2026-10-14T18:33:26.1031323-04:00", "applied": "2026-10-14T18:33:26.1031323-04:00", "boots": 0, "hours": 0, "failure": null}"#;
const SETS_FIXTURE: &str = r#"{"cpu": "Intel(R) Core(TM) Ultra 9 275HX", "lps": [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23], "sets": [{"name": "74optima", "ratios": [48, 48, 46, 46, 46, 46, 40, 40, 40, 40, 48, 48, 52, 46, 46, 46, 46, 46, 41, 41, 41, 41, 52, 52]}, {"name": "76optima", "ratios": [51, 47, 46, 46, 46, 46, 41, 41, 41, 41, 48, 47, 51, 46, 46, 46, 46, 46, 41, 41, 41, 41, 51, 52]}, {"name": "80maxsmart", "ratios": [50, 46, 46, 46, 46, 46, 39, 39, 39, 39, 47, 44, 51, 40, 46, 46, 46, 46, 39, 39, 39, 39, 52, 48]}, {"name": "csmart", "ratios": [48, 47, 45, 45, 45, 45, 36, 36, 36, 36, 48, 39, 48, 39, 46, 46, 46, 46, 32, 32, 32, 32, 49, 49]}, {"name": "csmart2", "ratios": [47, 47, 45, 45, 45, 45, 36, 36, 36, 36, 48, 39, 47, 39, 46, 46, 46, 46, 32, 32, 32, 32, 48, 48]}, {"name": "supercool", "ratios": [50, 47, 45, 45, 45, 45, 41, 41, 41, 41, 49, 47, 49, 44, 46, 46, 46, 46, 39, 39, 39, 39, 51, 50]}, {"name": "xsmart", "ratios": [52, 46, 46, 46, 46, 46, 36, 36, 36, 36, 52, 41, 50, 39, 46, 46, 46, 46, 33, 33, 33, 33, 51, 49]}]}"#;

fn is_p(lp: usize) -> bool {
    P_LPS.contains(&lp)
}

fn lp(position: usize) -> Lp {
    Lp(u32::try_from(position).expect("LP numbers are small"))
}

fn hx_lps() -> Vec<Lp> {
    (0..24).map(Lp).collect()
}

fn hx_lps_i32() -> Vec<i32> {
    (0..24).collect()
}

// KNOWLEDGE.md, measured on the 275HX: command 0x1C answers 0x3635 on P-cores 12 and 23, 0x3535 on the other P-cores,
// 0x2E2E on every E-core.
fn measured_reply(lp: usize) -> u32 {
    if lp == 12 || lp == 23 {
        0x3635
    } else if is_p(lp) {
        0x3535
    } else {
        0x2E2E
    }
}

// The same answers read as HWiNFO labels them (KNOWLEDGE.md): byte 1 the fused maximum, byte 0 the current ceiling.
fn measured(lp: usize) -> (u8, u8) {
    if lp == 12 || lp == 23 {
        (54, 53)
    } else if is_p(lp) {
        (53, 53)
    } else {
        (46, 46)
    }
}

fn measured_fused() -> Vec<i32> {
    (0..24).map(|lp| i32::from(measured(lp).0)).collect()
}

fn measured_current() -> Vec<i32> {
    (0..24).map(|lp| i32::from(measured(lp).1)).collect()
}

// «csmart2» with core 12 at ×56: above its fused ×54, so the profile is risky (Profile.Risky). Such a file can only
// date from before 15.10: the startup rules still treat it as risky (trial, disabled after a failure), but since
// DECISIONS #31 its ×56 is never written.
fn risky() -> Vec<i32> {
    let mut ratios = SAFE.to_vec();
    ratios[12] = 56;
    ratios
}

// What stands after `risky()` is applied (DECISIONS #31): every block from the profile except core 12, which keeps
// the ×53 the 275HX boots with (KNOWLEDGE.md: 0x3635) because its ×56 is refused, not clamped to ×54.
fn risky_applied() -> Vec<i32> {
    let mut ratios = SAFE.to_vec();
    ratios[12] = 53;
    ratios
}

// Profile.Startup note for a refusal, with Tuning.cs Set's range text for core 12 (fused ×54).
const RISKY_NOTE: &str = "профиль применён не полностью: потолок 56 вне допустимого: от ×27 до ×55";

fn list(values: &[i32]) -> String {
    let items: Vec<String> = values.iter().map(ToString::to_string).collect();
    format!("[{}]", items.join(", "))
}

// ---------------------------------------------------------------------------------------------------------------
// Temp folders
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug)]
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("coremap-spec-{}-{n}-{tag}", std::process::id()));
        fs::create_dir_all(&dir).expect("a temp folder is created");
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Cleanup only: a folder left behind in %TEMP% changes no result.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn store_in(dir: &Scratch) -> Store {
    Store::new(&dir.0, CPU, &hx_lps())
}

// ---------------------------------------------------------------------------------------------------------------
// Synthetic GetLogicalProcessorInformationEx(RelationAll) answers
// ---------------------------------------------------------------------------------------------------------------
// SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX: Relationship (DWORD) +0, Size (DWORD) +4, the union from +8.

fn put(record: &mut [u8], at: usize, bytes: &[u8]) {
    record[at..at + bytes.len()].copy_from_slice(bytes);
}

fn mask(lps: &[usize]) -> u64 {
    lps.iter().fold(0_u64, |m, &lp| m | (1_u64 << lp))
}

// PROCESSOR_RELATIONSHIP (core 0, package 3, die 5, module 7): Flags +8 (LTP_PC_SMT = 1), EfficiencyClass +9,
// Reserved[20], GroupCount +30, GROUP_AFFINITY { Mask +32, Group +40, Reserved[3] }: 48 bytes.
fn processor_record(relation: u32, smt: bool, class: u8, lps: &[usize], group: u16) -> Vec<u8> {
    let mut r = vec![0; 48];
    put(&mut r, 0, &relation.to_le_bytes());
    put(&mut r, 4, &48_u32.to_le_bytes());
    r[8] = u8::from(smt);
    r[9] = class;
    put(&mut r, 30, &1_u16.to_le_bytes());
    put(&mut r, 32, &mask(lps).to_le_bytes());
    put(&mut r, 40, &group.to_le_bytes());
    r
}

// CACHE_RELATIONSHIP (2): Level +8, Associativity +9, LineSize +10, CacheSize +12, Type +16 (0 unified, 1 instruction,
// 2 data), Reserved[18], GroupCount +38, GROUP_AFFINITY { Mask +40, Group +48, Reserved[3] }: 56 bytes.
fn cache_record(level: u8, kind: u32, lps: &[usize], group: u16) -> Vec<u8> {
    let mut r = vec![0; 56];
    put(&mut r, 0, &2_u32.to_le_bytes());
    put(&mut r, 4, &56_u32.to_le_bytes());
    r[8] = level;
    r[9] = 12;
    put(&mut r, 10, &64_u16.to_le_bytes());
    put(&mut r, 12, &(u32::from(level) << 20).to_le_bytes());
    put(&mut r, 16, &kind.to_le_bytes());
    put(&mut r, 38, &1_u16.to_le_bytes());
    put(&mut r, 40, &mask(lps).to_le_bytes());
    put(&mut r, 48, &group.to_le_bytes());
    r
}

// NUMA_NODE_RELATIONSHIP (1): NodeNumber +8, Reserved[18], GroupCount +30, GROUP_AFFINITY at +32: 48 bytes.
fn numa_record(lps: &[usize]) -> Vec<u8> {
    let mut r = vec![0; 48];
    put(&mut r, 0, &1_u32.to_le_bytes());
    put(&mut r, 4, &48_u32.to_le_bytes());
    put(&mut r, 30, &1_u16.to_le_bytes());
    put(&mut r, 32, &mask(lps).to_le_bytes());
    r
}

// GROUP_RELATIONSHIP (4): MaximumGroupCount +8, ActiveGroupCount +10, Reserved[20], PROCESSOR_GROUP_INFO at +32
// { MaximumProcessorCount +32, ActiveProcessorCount +33, Reserved[38], ActiveProcessorMask +72 }: 80 bytes.
fn group_record(lps: &[usize]) -> Vec<u8> {
    let count = u8::try_from(lps.len()).expect("one group holds at most 64");
    let mut r = vec![0; 80];
    put(&mut r, 0, &4_u32.to_le_bytes());
    put(&mut r, 4, &80_u32.to_le_bytes());
    put(&mut r, 8, &1_u16.to_le_bytes());
    put(&mut r, 10, &1_u16.to_le_bytes());
    r[32] = count;
    r[33] = count;
    put(&mut r, 72, &mask(lps).to_le_bytes());
    r
}

// The 275HX as Windows describes it: P-cores in efficiency class 1, E-cores in class 0, one L2 per P-core and one per
// E cluster, plus the records a parser must step over (NUMA node, L1, L3, package, group, die, modules).
fn hx_records() -> Vec<Vec<u8>> {
    let all: Vec<usize> = (0..24).collect();
    let mut records: Vec<Vec<u8>> = (0..24)
        .map(|lp| processor_record(0, false, u8::from(is_p(lp)), &[lp], 0))
        .collect();
    records.push(numa_record(&all));
    for lp in 0..24 {
        records.push(cache_record(1, 2, &[lp], 0));
        records.push(cache_record(1, 1, &[lp], 0));
    }
    records.extend(P_LPS.iter().map(|&lp| cache_record(2, 0, &[lp], 0)));
    records.extend(CLUSTERS.iter().map(|c| cache_record(2, 0, c, 0)));
    records.push(cache_record(3, 0, &all, 0));
    records.push(processor_record(3, false, 0, &all, 0));
    records.push(group_record(&all));
    records.push(processor_record(5, false, 0, &all, 0));
    records.extend(P_LPS.iter().map(|&lp| processor_record(7, false, 0, &[lp], 0)));
    records.extend(CLUSTERS.iter().map(|c| processor_record(7, false, 0, c, 0)));
    records
}

fn hx_buffer() -> Vec<u8> {
    hx_records().concat()
}

fn hx() -> Topology {
    Topology::parse(&hx_buffer()).expect("the 275HX layout parses")
}

// ---------------------------------------------------------------------------------------------------------------
// A fake CPU with the 275HX's measured answers, built from LP numbers alone (not from the library's topology)
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug)]
struct Bench {
    fused: Vec<u8>,
    current: Vec<u8>,
    deaf: Vec<bool>,          // the core never answers about its ceiling
    flaky: Vec<usize>,        // the next N questions to this core get no answer
    unpinnable: Vec<bool>,    // the thread cannot be pinned to this core: nothing is sent
    refuse: Vec<Option<i32>>, // a write to this core gets this answer code and changes nothing
    broken: Option<String>,   // every call fails with this driver error
    asks: Vec<usize>,         // cores asked, in order
    tells: Vec<(usize, u8)>,  // writes, in order
    peek: Option<PathBuf>,    // a file read at each write: what was on disk when the CPU was written
    peeked: Vec<String>,
}

impl Bench {
    fn hx() -> Self {
        Self {
            fused: (0..24).map(|lp| measured(lp).0).collect(),
            current: (0..24).map(|lp| measured(lp).1).collect(),
            deaf: vec![false; 24],
            flaky: vec![0; 24],
            unpinnable: vec![false; 24],
            refuse: vec![None; 24],
            broken: None,
            asks: Vec::new(),
            tells: Vec::new(),
            peek: None,
            peeked: Vec::new(),
        }
    }

    fn current_i32(&self) -> Vec<i32> {
        self.current.iter().map(|&c| i32::from(c)).collect()
    }
}

// Tuning.cs Tell: the CPU itself spreads a write to an E-core over its whole cluster.
fn members_of(core: usize) -> Vec<usize> {
    CLUSTERS
        .iter()
        .find(|c| c.contains(&core))
        .map_or_else(|| vec![core], |c| c.to_vec())
}

impl Cpu for Bench {
    fn ask(&mut self, core: usize) -> Result<Option<Limit>, HwError> {
        if let Some(cause) = &self.broken {
            return Err(HwError(cause.clone()));
        }
        self.asks.push(core);
        if self.flaky[core] > 0 {
            self.flaky[core] -= 1;
            return Ok(None);
        }
        Ok((!self.deaf[core]).then_some(Limit {
            fused: self.fused[core],
            current: self.current[core],
        }))
    }

    fn tell(&mut self, core: usize, ratio: Allowed) -> Result<Option<i32>, HwError> {
        let ratio = ratio.get();
        if let Some(cause) = &self.broken {
            return Err(HwError(cause.clone()));
        }
        if let Some(path) = &self.peek {
            self.peeked.push(fs::read_to_string(path).unwrap_or_default());
        }
        self.tells.push((core, ratio));
        if self.unpinnable[core] {
            return Ok(None);
        }
        if let Some(code) = self.refuse[core] {
            return Ok(Some(code));
        }
        for member in members_of(core) {
            self.current[member] = ratio;
        }
        Ok(Some(0))
    }
}

fn read_limits(cpu: &mut dyn Cpu, topo: &Topology) -> Limits {
    // The range depends on the CPU's name (base ratios, DECISIONS #34): the tests name the 275HX themselves.
    Limits::read_for(cpu, topo, CPU).expect("ceilings are read").0
}

fn limits_with(setup: impl FnOnce(&mut Bench)) -> Limits {
    let topo = hx();
    let mut bench = Bench::hx();
    setup(&mut bench);
    read_limits(&mut bench, &topo)
}

fn at_factory(bench: &mut Bench) {
    bench.current.clone_from(&bench.fused);
}

fn at_safe(bench: &mut Bench) {
    bench.current = SAFE
        .iter()
        .map(|&r| u8::try_from(r).expect("a ratio fits a byte"))
        .collect();
}

// ---------------------------------------------------------------------------------------------------------------
// A fake Windows event log
// ---------------------------------------------------------------------------------------------------------------

const KERNEL_POWER: &str = "Microsoft-Windows-Kernel-Power";
const EVENT_LOG: &str = "EventLog";
const WER: &str = "Microsoft-Windows-WER-SystemErrorReporting";
const WHEA: &str = "Microsoft-Windows-WHEA-Logger";

#[derive(Debug, Clone, Copy)]
struct Logged {
    provider: &'static str,
    id: u16,
    level: u8,
    at: Stamp,
}

fn event(provider: &'static str, id: u16, level: u8, at: Stamp) -> Logged {
    Logged {
        provider,
        id,
        level,
        at,
    }
}

#[derive(Debug, Default)]
struct Journal {
    events: Vec<Logged>,
    broken: Option<String>,
    asked: RefCell<Vec<Stamp>>, // `since` of every query
}

fn journal(events: &[Logged]) -> Journal {
    Journal {
        events: events.to_vec(),
        ..Journal::default()
    }
}

impl EventLog for Journal {
    fn first(&self, since: Stamp, condition: &str) -> Result<Option<Event>, String> {
        self.asked.borrow_mut().push(since);
        if let Some(cause) = &self.broken {
            return Err(cause.clone());
        }
        Ok(self
            .events
            .iter()
            .filter(|e| e.at >= since && satisfies(e, condition))
            .min_by_key(|e| e.at)
            .map(|e| Event {
                provider: e.provider.to_string(),
                id: e.id,
                at: e.at,
            }))
    }
}

// A small evaluator for the conditions of Profile.cs (FailureEvents, the 6006 query) and DECISIONS #31 (6006 or
// Kernel-Power 42): each clause is Provider[@Name='X'] with a disjunction of EventID=N and Level=N terms.
fn satisfies(e: &Logged, condition: &str) -> bool {
    let flat: String = condition.chars().filter(|c| !c.is_whitespace()).collect();
    flat.split("Provider[@Name='").skip(1).any(|clause| {
        let Some((provider, terms)) = clause.split_once('\'') else {
            return false;
        };
        provider == e.provider
            && (numbers_after(terms, "EventID=").contains(&u32::from(e.id))
                || numbers_after(terms, "Level=").contains(&u32::from(e.level)))
    })
}

fn numbers_after(text: &str, key: &str) -> Vec<u32> {
    text.match_indices(key)
        .filter_map(|(at, _)| {
            let digits: String = text[at + key.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().ok()
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Times and profile files written by hand in the C# writer's format
// ---------------------------------------------------------------------------------------------------------------

fn local(text: &str) -> Stamp {
    Stamp::parse_roundtrip(text).unwrap_or_else(|| panic!("{text} parses"))
}

fn saved_at() -> Stamp {
    local("2026-10-13T20:00:00")
}

fn applied_at() -> Stamp {
    local("2026-10-14T10:00:00")
}

// «Now» of every startup in these tests.
fn morning() -> Stamp {
    local("2026-10-15T09:00:00")
}

// A moment on 14.10, the day of the last application.
fn at(hhmm: &str) -> Stamp {
    local(&format!("2026-10-14T{hhmm}:00"))
}

#[derive(Debug, Clone)]
struct Draft {
    name: String,
    ratios: Vec<i32>,
    state: &'static str,
    applied: Stamp,
    boots: i32,
    hours: &'static str,
    failure: Option<String>,
}

fn draft(state: &'static str, ratios: &[i32]) -> Draft {
    Draft {
        name: "csmart2".into(),
        ratios: ratios.to_vec(),
        state,
        applied: applied_at(),
        boots: 0,
        hours: "0",
        failure: None,
    }
}

// Profile.Write field order with Json.cs separators.
fn json_of(d: &Draft) -> String {
    let failure = d
        .failure
        .as_ref()
        .map_or_else(|| "null".to_string(), |f| format!("\"{f}\""));
    format!(
        "{{\"cpu\": \"{CPU}\", \"name\": \"{}\", \"lps\": {}, \"ratios\": {}, \"state\": \"{}\", \"saved\": \"{}\", \
         \"applied\": \"{}\", \"boots\": {}, \"hours\": {}, \"failure\": {failure}}}",
        d.name,
        list(&hx_lps_i32()),
        list(&d.ratios),
        d.state,
        saved_at().roundtrip(),
        d.applied.roundtrip(),
        d.boots,
        d.hours,
    )
}

// LimitHold.Write: {cpu, time, lps, limits}; null entries stand for what Json.Ints reads as 0.
fn marker_text(cpu: &str, lps: &[i32], limits: &[Option<i32>]) -> String {
    let limits: Vec<String> = limits
        .iter()
        .map(|v| v.map_or_else(|| "null".to_string(), |v| v.to_string()))
        .collect();
    format!(
        "{{\"cpu\": \"{cpu}\", \"time\": \"2026-10-14 10:00:00\", \"lps\": {}, \"limits\": [{}]}}",
        list(lps),
        limits.join(", ")
    )
}

fn as_marker(ratios: &[i32]) -> Vec<Option<i32>> {
    ratios.iter().copied().map(Some).collect()
}

// A data folder, the 275HX topology, the fake CPU and the ceilings read from it.
struct Rig {
    dir: Scratch,
    topo: Topology,
    bench: Bench,
    limits: Limits,
    store: Store,
}

impl Rig {
    fn new(tag: &str, setup: impl FnOnce(&mut Bench)) -> Self {
        let dir = Scratch::new(tag);
        let topo = hx();
        let mut bench = Bench::hx();
        setup(&mut bench);
        let limits = read_limits(&mut bench, &topo);
        bench.asks.clear();
        let store = store_in(&dir);
        Self {
            dir,
            topo,
            bench,
            limits,
            store,
        }
    }

    fn with(tag: &str, d: &Draft, setup: impl FnOnce(&mut Bench)) -> Self {
        let rig = Self::new(tag, setup);
        rig.put(&json_of(d));
        rig
    }

    fn put(&self, text: &str) {
        fs::write(self.store.profile_path(), text).expect("profile.json is written");
    }

    fn raw(&self) -> Vec<u8> {
        fs::read(self.store.profile_path()).expect("profile.json is there")
    }

    fn on_disk(&self) -> Profile {
        self.store
            .read_profile()
            .expect("profile.json reads")
            .expect("profile.json is ours")
    }

    fn startup(&mut self, log: &Journal) -> Startup {
        profile::startup(
            &self.store,
            &mut self.limits,
            &mut self.bench,
            &self.topo,
            log,
            &morning,
        )
        .expect("profile.json reads")
    }

    fn marker(&self) -> PathBuf {
        self.dir.file("limits-held.json")
    }

    fn recover(&mut self) -> Recovery {
        let path = self.marker();
        marker::recover(&path, CPU, &hx_lps(), &mut self.limits, &mut self.bench, &self.topo)
    }
}

// ===============================================================================================================
// 1. Mailbox: request layout and answers
// ===============================================================================================================

#[test]
fn mailbox_request_has_the_csharp_layout() {
    // Tuning.cs OcMailbox.Query: 1<<63 | param2<<48 | param1<<40 | command<<32 | data; parameters are 0 here.
    assert_eq!(mailbox::request(0x1C, 0), 0x8000_001C_0000_0000);
    assert_eq!(mailbox::request(0x1D, 45), 0x8000_001D_0000_002D);
    assert_eq!(mailbox::request(0x1D, 0xFFFF_FFFF), 0x8000_001D_FFFF_FFFF);
    for command in [0x00_u8, 0x01, 0x1C, 0x1D, 0xFF] {
        for data in [0_u32, 8, 0x2E, 0x8000_0000, u32::MAX] {
            let r = mailbox::request(command, data);
            assert_eq!(r >> 63, 1, "bit 63 starts the request");
            assert_eq!((r >> 40) & 0x7F_FFFF, 0, "param1 and param2 are zero");
            assert_eq!((r >> 32) & 0xFF, u64::from(command), "command in bits 32-39");
            assert_eq!(r & 0xFFFF_FFFF, u64::from(data), "data in bits 0-31");
        }
    }
}

#[test]
fn mailbox_reply_is_error_in_bits_32_to_39_and_data_in_bits_0_to_31() {
    // Tuning.cs: error = answer >> 32 & 0xFF, data = (uint)answer; the 275HX answers measured in KNOWLEDGE.md.
    assert_eq!(mailbox::reply(0x0000_0000_0000_3635), (0, 0x3635));
    assert_eq!(mailbox::reply(0x0000_0000_0000_2E2E), (0, 0x2E2E));
    assert_eq!(mailbox::reply(0x0000_0007_0000_3535), (7, 0x3535));
    assert_eq!(
        mailbox::reply(0xFFFF_FF05_1234_5678),
        (5, 0x1234_5678),
        "bits 40-63 are not the error code"
    );
}

// The register MSR 0x150 as the CPU behaves: a request with bit 63 is answered at once with bit 63 clear; the answer
// to 0x1C depends on the processor the thread runs on (the measured 275HX answers).
#[derive(Debug, Default)]
struct Register {
    value: u64,
    busy_reads: usize, // this many reads still show bit 63: someone else's request is running
    error: u8,         // the error code of every answer
    stuck: bool,       // the CPU never clears bit 63 after a write
    writes: Vec<(u32, u64, u32)>, // (register, value, processor the write ran on)
    reads: Vec<u32>,
}

#[derive(Debug)]
struct FakeMsr(Rc<RefCell<Register>>);

fn processor_now() -> u32 {
    // SAFETY: GetCurrentProcessorNumber has no parameters and no preconditions; it only reports where the calling
    // thread runs.
    unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessorNumber() }
}

impl Msr for FakeMsr {
    fn read(&mut self, msr: u32) -> Result<u64, HwError> {
        let mut register = self.0.borrow_mut();
        register.reads.push(msr);
        if register.busy_reads > 0 {
            register.busy_reads -= 1;
            return Ok(1 << 63);
        }
        Ok(register.value)
    }

    fn write(&mut self, msr: u32, value: u64) -> Result<(), HwError> {
        let processor = processor_now();
        let mut register = self.0.borrow_mut();
        register.writes.push((msr, value, processor));
        register.value = if register.stuck {
            value
        } else {
            let data = if (value >> 32) & 0xFF == 0x1C {
                measured_reply(usize::try_from(processor).expect("processor numbers are small"))
            } else {
                0
            };
            (u64::from(register.error) << 32) | u64::from(data)
        };
        Ok(())
    }
}

fn gate(tag: &str) -> String {
    format!("Local\\CoreMap.Mailbox.Spec.{}.{tag}", std::process::id())
}

fn mailbox_cpu(tag: &str, topo: &Topology) -> (MailboxCpu, Rc<RefCell<Register>>) {
    let register = Rc::new(RefCell::new(Register::default()));
    let cpu = MailboxCpu::new(Box::new(FakeMsr(Rc::clone(&register))), &gate(tag), topo).expect("the gate is created");
    (cpu, register)
}

#[test]
fn mailbox_reads_the_measured_answers_on_the_core_asked() {
    // Tuning.cs Ask: pin the thread to the core's LP, command 0x1C on MSR 0x150, fused = byte 1, current = byte 0;
    // the answers measured on the 275HX (KNOWLEDGE.md).
    let topo = hx();
    let (mut cpu, register) = mailbox_cpu("answers", &topo);
    let cores = [0_usize, 2, 12, 13, 21, 23];
    for core in cores {
        let (fused, current) = measured(core);
        assert_eq!(
            cpu.ask(core).expect("no driver error"),
            Some(Limit { fused, current }),
            "core {core}"
        );
    }
    let register = register.borrow();
    assert_eq!(register.writes.len(), cores.len());
    for (&core, &(msr, value, processor)) in cores.iter().zip(&register.writes) {
        assert_eq!(msr, 0x150);
        assert_eq!(value, 0x8000_001C_0000_0000);
        assert_eq!(
            processor,
            u32::try_from(core).expect("small"),
            "the request for core {core} runs on its own LP"
        );
    }
    assert!(register.reads.iter().all(|&msr| msr == 0x150));
}

#[test]
fn mailbox_writes_the_ceiling_in_byte_0_with_command_0x1d_on_the_core() {
    // Tuning.cs Tell: pin to lps[index], command 0x1D, ratio as data.
    let topo = hx();
    // The write path is reachable only through Limits::set (the range check), since the review of 14.10: the
    // ratio type the mailbox takes cannot be built outside the library.
    let (mut cpu, register) = mailbox_cpu("write", &topo);
    let (mut limits, _) = Limits::read(&mut cpu, &topo).expect("the measured answers are read");
    register.borrow_mut().writes.clear();
    let _ = limits.set(&mut cpu, &topo, 0, 45); // the fake CPU does not change its answer; only the request counts
    let _ = limits.set(&mut cpu, &topo, 5, 40);
    let writes: Vec<(u32, u64, u32)> = register
        .borrow()
        .writes
        .iter()
        .filter(|w| (w.1 >> 32) & 0xFF == 0x1D)
        .copied()
        .collect();
    assert_eq!(
        writes,
        [(0x150, 0x8000_001D_0000_002D, 0), (0x150, 0x8000_001D_0000_0028, 5)]
    );
}

#[test]
fn mailbox_refusals_and_silence_are_not_answers() {
    // Tuning.cs: error != 0 -> Ask fails (the core did not answer); no clear bit 63 within the polls -> error -1.
    let topo = hx();
    let (mut cpu, register) = mailbox_cpu("refused", &topo);
    let (mut limits, _) = Limits::read(&mut cpu, &topo).expect("read while answers are clean");
    register.borrow_mut().error = 3;
    assert_eq!(
        cpu.ask(0).expect("no driver error"),
        None,
        "a refused read is no answer"
    );
    let refused = limits.set(&mut cpu, &topo, 0, 45);
    assert!(
        refused.is_some(),
        "a refused write is not reported accepted: {refused:?}"
    );

    let (mut cpu, register) = mailbox_cpu("stuck", &topo);
    let (mut limits, _) = Limits::read(&mut cpu, &topo).expect("read before the CPU gets stuck");
    register.borrow_mut().stuck = true;
    assert_eq!(cpu.ask(0).expect("no driver error"), None);
    register.borrow_mut().value = 0; // the mailbox is free again for the write
    let silent = limits.set(&mut cpu, &topo, 0, 45);
    assert!(
        silent.as_deref().is_some_and(|t| t.contains("не ответил")),
        "no answer to the write is said as such: {silent:?}"
    );
}

#[test]
fn mailbox_waits_for_a_running_request_and_never_writes_over_it() {
    // DECISIONS #31 (d): the busy bit is checked before writing (C# wrote at once, inventory B3).
    let topo = hx();
    let (mut cpu, register) = mailbox_cpu("busy", &topo);
    register.borrow_mut().busy_reads = 3;
    assert_eq!(
        cpu.ask(12).expect("no driver error"),
        Some(Limit { fused: 54, current: 53 })
    );
    assert_eq!(register.borrow().writes.len(), 1, "written once the mailbox was free");

    let (mut cpu, register) = mailbox_cpu("always-busy", &topo);
    let (mut limits, _) = Limits::read(&mut cpu, &topo).expect("read while the mailbox is free");
    register.borrow_mut().writes.clear();
    register.borrow_mut().busy_reads = usize::MAX;
    let read = cpu.ask(0);
    let written = limits.set(&mut cpu, &topo, 0, 45);
    assert!(
        register.borrow().writes.is_empty(),
        "nothing is written over a request still running"
    );
    assert!(!matches!(read, Ok(Some(_))), "{read:?}");
    assert!(written.is_some(), "{written:?}");
}

#[test]
fn mailbox_refuses_when_its_gate_is_held_elsewhere() {
    // DECISIONS #31 (d): no request without the gate mutex (C# went ahead after 2 s, inventory B3).
    let topo = hx();
    let name = gate("held");
    let (signal_taken, taken) = mpsc::channel();
    let (signal_finish, finish) = mpsc::channel::<()>();
    let holder = std::thread::spawn({
        let name = name.clone();
        move || {
            let (mutex, existed) = named::create(&name, true).expect("the gate is created");
            signal_taken.send(!existed).expect("the test waits for the gate");
            // Hold the gate until the test has made its request; a closed channel means the same.
            let _ = finish.recv();
            mutex.release();
        }
    });
    assert!(
        taken.recv().expect("the holder reports"),
        "the holder created and owns the gate"
    );
    let register = Rc::new(RefCell::new(Register::default()));
    let mut cpu = MailboxCpu::new(Box::new(FakeMsr(Rc::clone(&register))), &name, &topo).expect("the gate is opened");
    let answer = cpu.ask(0);
    signal_finish.send(()).expect("the holder waits");
    holder.join().expect("the holder ends");
    assert!(register.borrow().writes.is_empty(), "no request without the gate");
    assert!(!matches!(answer, Ok(Some(_))), "{answer:?}");
}

// ===============================================================================================================
// 2. Topology
// ===============================================================================================================

fn assert_hx(topo: &Topology) {
    assert_eq!(topo.cores.len(), 24);
    assert_eq!(topo.lps(), hx_lps(), "cores in LP order");
    for (position, core) in topo.cores.iter().enumerate() {
        assert_eq!(core.lp, lp(position));
        assert_eq!(
            core.kind,
            if is_p(position) { Kind::P } else { Kind::E },
            "kind of LP {position}"
        );
    }
    let physical: BTreeSet<usize> = topo.cores.iter().map(|c| c.physical).collect();
    assert_eq!(
        physical.len(),
        24,
        "no SMT on the 275HX: every LP is its own physical core"
    );
    let names: Vec<&str> = topo.blocks.iter().map(|b| b.name.as_str()).collect();
    let expected: Vec<&str> = HX_BLOCKS.iter().map(|&(name, _)| name).collect();
    assert_eq!(names, expected);
    for (block, &(name, members)) in topo.blocks.iter().zip(&HX_BLOCKS) {
        assert_eq!(block.members, members, "members of {name}");
        assert_eq!(block.first, members[0], "first core of {name}");
        assert_eq!(block.lp, lp(members[0]), "LP of {name}");
        assert_eq!(
            block.kind,
            if members.len() == 1 { Kind::P } else { Kind::E },
            "kind of {name}"
        );
    }
    for core in 0..24 {
        let (name, _) = HX_BLOCKS
            .iter()
            .find(|(_, members)| members.contains(&core))
            .expect("every core is in a block");
        assert_eq!(topo.block_of(core).name, *name, "block of core {core}");
    }
    let mut clusters = BTreeSet::new();
    for cluster in CLUSTERS {
        let id = topo.cores[cluster[0]]
            .cluster
            .expect("E-cores with a shared L2 are a cluster");
        assert!(cluster.iter().all(|&core| topo.cores[core].cluster == Some(id)));
        assert!(clusters.insert(id), "each shared L2 is its own cluster");
    }
    assert!(
        P_LPS.iter().all(|&core| topo.cores[core].cluster.is_none()),
        "a P-core's L2 is its own"
    );
}

#[test]
fn topology_of_the_275hx_is_24_cores_in_12_blocks() {
    // Topology.cs Read (offsets, P = highest efficiency class, sorted by LP), Block.cs BlockMap (E cluster = one
    // block, name «2–5»), inventory 4.4.
    assert_hx(&hx());
}

#[test]
fn topology_does_not_depend_on_the_order_of_records() {
    // Topology.cs: entries are sorted by LP; clusters are numbered by their first LP.
    let mut records = hx_records();
    records.reverse();
    assert_hx(&Topology::parse(&records.concat()).expect("the reversed answer parses"));
}

#[test]
fn smt_siblings_of_one_core_form_one_block() {
    // DECISIONS #31 (b): blocks group the logical processors of one physical core (C# made each its own block, B15).
    let mut records = vec![
        processor_record(0, true, 1, &[0, 1], 0),
        processor_record(0, true, 1, &[2, 3], 0),
    ];
    records.extend((4..=8).map(|lp| processor_record(0, false, 0, &[lp], 0)));
    records.extend([
        cache_record(2, 0, &[0, 1], 0),
        cache_record(2, 0, &[2, 3], 0),
        cache_record(2, 0, &[4, 5, 6, 7], 0),
        cache_record(2, 0, &[8], 0),
        cache_record(3, 0, &[0, 1, 2, 3, 4, 5, 6, 7, 8], 0),
    ]);
    let topo = Topology::parse(&records.concat()).expect("the layout parses");
    assert_eq!(topo.lps(), (0..9).map(Lp).collect::<Vec<_>>());
    assert_eq!(topo.cores[0].physical, topo.cores[1].physical);
    assert_ne!(topo.cores[1].physical, topo.cores[2].physical);
    assert!((0..4).all(|core| topo.cores[core].kind == Kind::P));
    let members: Vec<&[usize]> = topo.blocks.iter().map(|b| b.members.as_slice()).collect();
    let expected: [&[usize]; 4] = [&[0, 1], &[2, 3], &[4, 5, 6, 7], &[8]];
    assert_eq!(members, expected);
    assert_eq!(topo.block_of(1).first, 0);
    assert_eq!(topo.block_of(3).first, 2);
    assert_eq!(
        topo.block_of(8).members,
        [8],
        "a lone E-core with its own L2 is a block by itself"
    );
}

#[test]
fn a_cpu_without_e_cores_has_one_block_per_core() {
    // Topology.cs: P = the highest efficiency class (all, when all are equal); Block.cs: only E-cores are grouped.
    let mut records: Vec<Vec<u8>> = (0..4).map(|lp| processor_record(0, false, 0, &[lp], 0)).collect();
    records.push(cache_record(2, 0, &[0, 1], 0));
    records.push(cache_record(2, 0, &[2, 3], 0));
    let topo = Topology::parse(&records.concat()).expect("the layout parses");
    assert!(topo.cores.iter().all(|c| c.kind == Kind::P));
    let names: Vec<&str> = topo.blocks.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["0", "1", "2", "3"]);
}

#[test]
fn processor_groups_beyond_the_first_are_refused() {
    // Topology.cs: a core record with Group != 0 -> NotSupportedException (more than 64 LPs are not supported).
    let mut records = hx_records();
    records[5] = processor_record(0, false, 0, &[5], 1);
    assert!(Topology::parse(&records.concat()).is_err());
}

#[test]
fn an_answer_cut_inside_a_record_is_refused() {
    // rust\AGENTS.md "Correct in reality": a cut answer must not become a topology of the first four cores.
    let buffer = hx_buffer();
    assert!(Topology::parse(&buffer[..4 * 48 + 20]).is_err());
}

#[test]
fn a_record_of_size_zero_does_not_hang_the_parser() {
    // Topology.cs steps by each record's Size; a broken Size 0 must end in a refusal, not an endless loop.
    let mut buffer = cache_record(1, 2, &[0], 0);
    put(&mut buffer, 4, &0_u32.to_le_bytes());
    buffer.extend(hx_buffer());
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        // After a timeout nobody listens; the result then has nowhere to go.
        let _ = done.send(Topology::parse(&buffer).is_err());
    });
    let refused = finished
        .recv_timeout(Duration::from_secs(5))
        .expect("the parser returns within 5 s, no panic");
    assert!(refused, "a record of size 0 is a broken answer");
}

// ===============================================================================================================
// 3. Ceilings
// ===============================================================================================================

#[test]
fn sim_answers_like_the_measured_275hx() {
    // KNOWLEDGE.md answers to 0x1C; Tuning.cs simulation: P 53, E 46, fused 54 at LP 12 and 23.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    let (limits, unread) = Limits::read(&mut sim, &topo).expect("simulated ceilings are read");
    assert_eq!(unread, None);
    assert_eq!(limits.fused(), measured_fused().as_slice());
    assert_eq!(limits.current(), measured_current().as_slice());
    assert!((0..24).all(|core| limits.known(core)));
}

#[test]
fn sim_reset_returns_to_what_a_rebooted_275hx_answers() {
    // sim.rs: reset_to_factory = sleep or reboot; after a reboot the 275HX answers the measured 0x3635 at LP 12:
    // ceiling ×53 under a fused ×54. Core 12 is raised to its fused ×54 so that a reset to fused instead of to the boot
    // value would show.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    let mut limits = read_limits(&mut sim, &topo);
    for (core, ratio) in [(0, 45), (12, 54), (2, 40)] {
        assert_eq!(limits.set(&mut sim, &topo, core, ratio), None);
    }
    sim.reset_to_factory();
    assert_eq!(limits.refresh(&mut sim, &topo), None);
    assert_eq!(limits.current(), measured_current().as_slice());
}

#[test]
fn choices_go_from_one_above_fused_down_to_the_base_ratio_of_the_core_type() {
    // DECISIONS #34, the owner's word of 15.10: the top is fused + 1 («от заводского плюс один не больше»), the bottom
    // the base ratio of the core's type - on the 275HX P x27 (2.7 GHz), E x21 (2.1 GHz): Intel's turbo range starts at
    // base and BIOS offers no per-core ceiling below it. Descending, as Tuning.cs Choices.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    let limits = read_limits(&mut sim, &topo);
    for (core, top, bottom) in [(0, 54, 27), (12, 55, 27), (23, 55, 27), (2, 47, 21), (21, 47, 21)] {
        let choices = limits.choices(core);
        assert_eq!(choices.first(), Some(&top), "core {core}: one above fused: {choices:?}");
        assert_eq!(
            choices.last(),
            Some(&bottom),
            "core {core}: the base ratio: {choices:?}"
        );
        assert_eq!(limits.floor(core), bottom);
        assert!(
            choices.windows(2).all(|w| w[0] == w[1] + 1),
            "consecutive and descending: {choices:?}"
        );
    }
}

#[test]
fn a_cpu_not_in_the_table_keeps_fourteen_below_fused_and_never_below_eight() {
    // DECISIONS #34: without a known base ratio the bottom is the rule of 12.10 - fused - 14, never below x8 (LFM).
    let topo = hx();
    let mut bench = Bench::hx();
    (bench.fused[1], bench.current[1]) = (10, 10);
    (bench.fused[10], bench.current[10]) = (8, 8);
    let limits = Limits::read_for(&mut bench, &topo, "Some other CPU").expect("read").0;
    assert_eq!(limits.choices(0), (39..=54).rev().collect::<Vec<i32>>());
    assert_eq!(limits.choices(2), (32..=47).rev().collect::<Vec<i32>>());
    assert_eq!(limits.choices(1), (8..=11).rev().collect::<Vec<i32>>());
    assert_eq!(limits.choices(10), [9, 8]);
}

#[test]
fn a_base_ratio_above_a_cores_fused_maximum_falls_back_to_the_old_rule() {
    // A 275HX answering fused x20 on a P core (base x27 would be above it): the bottom is fused - 14, not below x8.
    let topo = hx();
    let mut bench = Bench::hx();
    (bench.fused[0], bench.current[0]) = (20, 20);
    let limits = read_limits(&mut bench, &topo);
    assert_eq!(limits.choices(0), (8..=21).rev().collect::<Vec<i32>>());
}

#[test]
fn set_refuses_values_outside_the_range_with_the_csharp_text() {
    // Tuning.cs Set: "потолок N вне допустимого: от ×min до ×max"; nothing is written. DECISIONS #34 (15.10): the range
    // is base..fused+1, so two steps above fused and one below base are refused.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    let mut limits = read_limits(&mut sim, &topo);
    for (core, ratio, text) in [
        (0, 55, "потолок 55 вне допустимого: от ×27 до ×54"),
        (0, 26, "потолок 26 вне допустимого: от ×27 до ×54"),
        (0, 8, "потолок 8 вне допустимого: от ×27 до ×54"),
        (2, 48, "потолок 48 вне допустимого: от ×21 до ×47"),
        (2, 20, "потолок 20 вне допустимого: от ×21 до ×47"),
        (12, 56, "потолок 56 вне допустимого: от ×27 до ×55"),
    ] {
        assert_eq!(limits.set(&mut sim, &topo, core, ratio).as_deref(), Some(text));
    }
    assert_eq!(sim.writes, 0);
    assert_eq!(limits.current(), measured_current().as_slice());
}

#[test]
fn set_accepts_both_ends_of_the_range() {
    // Tuning.cs Set: any value of Choices is written; DECISIONS #34: the ends are the base ratio and fused + 1. Each
    // write moves the ceiling, so a write that never reached the CPU would show.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    let mut limits = read_limits(&mut sim, &topo);
    for (core, ratio) in [(12, 55), (1, 27), (6, 21), (1, 54), (6, 47)] {
        assert_eq!(limits.set(&mut sim, &topo, core, ratio), None, "core {core} ×{ratio}");
        assert_eq!(limits.current()[core], ratio);
        assert_eq!(i32::from(sim.current()[core]), ratio);
    }
    assert_eq!(sim.writes, 5);
}

#[test]
fn a_write_to_an_e_core_sets_its_whole_cluster_with_one_write() {
    // Tuning.cs Set: "ставит потолок ядру, а с ним — всему его блоку"; Block.cs: an E cluster is one block.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    let mut limits = read_limits(&mut sim, &topo);
    assert_eq!(
        limits.set(&mut sim, &topo, 3, 40),
        None,
        "through a member that is not the first"
    );
    assert_eq!(sim.writes, 1);
    assert_eq!(limits.set(&mut sim, &topo, 21, 33), None);
    assert_eq!(sim.writes, 2);
    let mut expected = measured_current();
    expected[2..=5].fill(40);
    expected[18..=21].fill(33);
    assert_eq!(limits.current(), expected.as_slice());
    assert_eq!(
        sim.current().iter().map(|&c| i32::from(c)).collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn apply_writes_only_the_blocks_that_differ() {
    // Tuning.cs Apply: Set only where Current != ratio; DECISIONS #31 (c): one write per block.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    let mut limits = read_limits(&mut sim, &topo);
    let mut ratios = measured_current();
    ratios[0] = 50;
    ratios[6..=9].fill(40);
    ratios[12] = 54; // its fused maximum, the highest allowed since DECISIONS #31
    assert_eq!(limits.apply(&mut sim, &topo, &ratios), None);
    assert_eq!(sim.writes, 3);
    assert_eq!(limits.current(), ratios.as_slice());
    assert_eq!(sim.current().iter().map(|&c| i32::from(c)).collect::<Vec<_>>(), ratios);
    assert_eq!(limits.apply(&mut sim, &topo, &ratios), None);
    assert_eq!(sim.writes, 3, "a set that already stands is not written again");
}

#[test]
fn a_core_that_never_answered_is_neither_shown_nor_written() {
    // Tuning.cs: Fused 0 = never answered, Known false; Same and Risky skip it; Apply skips it; Set refuses it.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    sim.set_deaf(11, true);
    let (mut limits, unread) = Limits::read(&mut sim, &topo).expect("the other cores are read");
    assert_eq!(unread.as_deref(), Some("процессор не ответил о потолке ядер: 11"));
    assert!(!limits.known(11));
    assert!((0..24).filter(|&core| core != 11).all(|core| limits.known(core)));
    assert_eq!(limits.fused()[11], 0);
    assert!(limits.choices(11).is_empty(), "nothing to offer for an unknown core");
    let mut ratios = measured_current();
    ratios[11] = 99;
    assert!(limits.same(&ratios), "an unknown core does not count in the comparison");
    assert!(
        !limits.above_factory(&ratios),
        "an unknown core does not make a set risky"
    );
    ratios[11] = 40;
    assert_eq!(limits.apply(&mut sim, &topo, &ratios), None);
    assert_eq!(
        limits.set(&mut sim, &topo, 11, 40).as_deref(),
        Some("процессор не сообщил потолок этого ядра")
    );
    assert_eq!(sim.writes, 0);
}

#[test]
fn unread_cores_are_listed_by_lp() {
    // Tuning.cs Read: "процессор не ответил о потолке ядер: " + LPs joined by ", ".
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    sim.set_deaf(11, true);
    sim.set_deaf(15, true);
    let (_, unread) = Limits::read(&mut sim, &topo).expect("the other cores are read");
    assert_eq!(unread.as_deref(), Some("процессор не ответил о потолке ядер: 11, 15"));
}

#[test]
fn every_kind_of_refused_write_has_the_csharp_text() {
    // Tuning.cs Set: pin failure, no answer after the write, a different ceiling read back, a driver error.
    let topo = hx();
    let mut bench = Bench::hx();
    let mut limits = read_limits(&mut bench, &topo);
    bench.refuse[1] = Some(17);
    assert_eq!(
        limits.set(&mut bench, &topo, 1, 45).as_deref(),
        Some("процессор не принял потолок 45 для ядра 1: код ответа 17, действующий потолок остался 53")
    );
    assert_eq!(limits.current()[1], 53);
    bench.unpinnable[10] = true;
    assert_eq!(
        limits.set(&mut bench, &topo, 10, 45).as_deref(),
        Some("не удалось привязать поток к процессору 10: потолок не записан")
    );
    bench.deaf[11] = true;
    assert_eq!(
        limits.set(&mut bench, &topo, 11, 45).as_deref(),
        Some("потолок 45 для ядра 11 записан, но процессор не ответил, принял ли он его")
    );
    bench.broken = Some("драйвер PawnIO не ответил".into());
    assert_eq!(
        limits.set(&mut bench, &topo, 13, 45).as_deref(),
        Some("драйвер PawnIO не ответил")
    );
}

#[test]
fn apply_reports_the_last_refusal_and_still_writes_the_rest() {
    // Tuning.cs Apply: refusal = Set(...) ?? refusal, over all cores.
    let topo = hx();
    let mut bench = Bench::hx();
    let mut limits = read_limits(&mut bench, &topo);
    bench.refuse[0] = Some(17);
    bench.refuse[22] = Some(9);
    let mut ratios = measured_current();
    ratios[0] = 50;
    ratios[13] = 50;
    ratios[22] = 50;
    assert_eq!(
        limits.apply(&mut bench, &topo, &ratios).as_deref(),
        Some("процессор не принял потолок 50 для ядра 22: код ответа 9, действующий потолок остался 53")
    );
    assert_eq!(limits.current()[13], 50, "the block between two refusals is written");
}

#[test]
fn after_a_write_only_the_written_block_is_read_back() {
    // DECISIONS #31 (c) (C# re-read all 24 cores after each write, inventory B16).
    let topo = hx();
    let mut bench = Bench::hx();
    let mut limits = read_limits(&mut bench, &topo);
    bench.asks.clear();
    assert_eq!(limits.set(&mut bench, &topo, 2, 40), None);
    let asked: BTreeSet<usize> = bench.asks.iter().copied().collect();
    assert!(asked.contains(&2), "the written core is read back: {asked:?}");
    assert!(
        asked.is_subset(&BTreeSet::from([2, 3, 4, 5])),
        "only block 2–5 is read: {asked:?}"
    );
    assert!(limits.current()[2..=5].iter().all(|&c| c == 40));
}

#[test]
fn a_core_is_asked_three_times_before_it_counts_as_unread() {
    // Tuning.cs: Attempts = 3.
    let topo = hx();
    let mut bench = Bench::hx();
    bench.flaky[0] = 2;
    bench.flaky[1] = 3;
    let (limits, unread) = Limits::read(&mut bench, &topo).expect("read");
    assert!(limits.known(0), "two missed answers, then one: the core is read");
    assert!(!limits.known(1));
    assert_eq!(unread.as_deref(), Some("процессор не ответил о потолке ядер: 1"));
    assert_eq!(bench.asks.iter().filter(|&&core| core == 1).count(), 3);
}

#[test]
fn fused_is_learned_once_and_a_late_core_is_learned_later() {
    // Tuning.cs Read: "if (Fused[i] == 0) Fused[i] = fused" - read once, from the first answer.
    let topo = hx();
    let mut bench = Bench::hx();
    bench.deaf[11] = true;
    let mut limits = read_limits(&mut bench, &topo);
    assert!(!limits.known(11));
    bench.deaf[11] = false;
    bench.fused[0] = 60;
    assert_eq!(limits.refresh(&mut bench, &topo), None);
    assert!(limits.known(11));
    assert_eq!(limits.fused()[11], 53);
    assert_eq!(
        limits.fused()[0],
        53,
        "a later answer does not change the fused maximum"
    );
}

#[test]
fn a_known_core_that_stops_answering_is_rewritten_by_apply() {
    // Tuning.cs Apply: "Потолок, о котором процессор не ответил, пишется и тогда, когда по памяти программы он уже такой".
    let topo = hx();
    let mut bench = Bench::hx();
    let mut limits = read_limits(&mut bench, &topo);
    bench.deaf[0] = true;
    bench.tells.clear();
    assert_eq!(
        limits.apply(&mut bench, &topo, &measured_current()).as_deref(),
        Some("потолок 53 для ядра 0 записан, но процессор не ответил, принял ли он его")
    );
    assert_eq!(bench.tells, [(0, 53)]);
}

#[test]
fn refresh_sees_a_foreign_change_and_keeps_what_an_unanswered_core_had() {
    // Tuning.cs Refresh/Read: a failed answer keeps the known Current and is reported.
    let topo = hx();
    let mut sim = SimCpu::new(&topo);
    let mut limits = read_limits(&mut sim, &topo);
    sim.foreign_write(0, 45);
    assert_eq!(limits.refresh(&mut sim, &topo), None);
    assert_eq!(limits.current()[0], 45);
    sim.set_deaf(1, true);
    sim.foreign_write(1, 44);
    assert_eq!(
        limits.refresh(&mut sim, &topo).as_deref(),
        Some("процессор не ответил о потолке ядер: 1")
    );
    assert_eq!(limits.current()[1], 53);
    assert!(limits.known(1));
}

#[test]
fn a_driver_error_fails_the_read_and_is_the_refresh_reason() {
    // Tuning.cs: SensorException from Read propagates out of the constructor; Refresh returns its message.
    let topo = hx();
    let mut bench = Bench::hx();
    bench.broken = Some("PawnIO не открылся, код 0x80070005".into());
    assert_eq!(
        Limits::read(&mut bench, &topo).err(),
        Some(HwError("PawnIO не открылся, код 0x80070005".into()))
    );
    bench.broken = None;
    let mut limits = read_limits(&mut bench, &topo);
    bench.broken = Some("драйвер вернул пустой ответ".into());
    assert_eq!(
        limits.refresh(&mut bench, &topo).as_deref(),
        Some("драйвер вернул пустой ответ")
    );
}

// ===============================================================================================================
// 4. Startup rules (Profile.Startup)
// ===============================================================================================================

#[test]
fn startup_without_a_profile_does_nothing() {
    // Profile.Startup: !Exists -> return; the log is not read, nothing is written.
    let mut rig = Rig::new("start-none", |_| {});
    let log = journal(&[]);
    assert_eq!(
        rig.startup(&log),
        Startup {
            profile: None,
            note: None,
            at_startup: None
        }
    );
    assert_eq!(rig.bench.tells, NO_WRITES, "nothing is written to the CPU");
    assert!(log.asked.borrow().is_empty());
    assert!(!rig.store.profile_path().exists());
}

#[test]
fn startup_ignores_a_profile_of_another_cpu_or_layout() {
    // Profile.Read: a different cpu or lps -> no profile, silently.
    let text = json_of(&draft(profile::TRIAL, &risky()));
    for (tag, other) in [
        ("cpu", text.replace(CPU, OTHER_CPU)),
        ("lps", text.replace("\"lps\": [0, 1,", "\"lps\": [1, 0,")),
    ] {
        let mut rig = Rig::new(&format!("start-other-{tag}"), |_| {});
        rig.put(&other);
        let out = rig.startup(&journal(&[]));
        assert_eq!(out.profile, None, "{tag}");
        assert!(rig.bench.tells.is_empty(), "{tag}");
        assert_eq!(rig.raw(), other.as_bytes(), "{tag}: the file is left as it is");
    }
}

#[test]
fn startup_leaves_a_disabled_profile_alone() {
    // Profile.Startup: State == Failed -> return before the log and before any write.
    let mut d = draft(profile::FAILED, &risky());
    d.failure = Some("синий экран 13.10 22:00 — после применения профиля 13.10 21:00".into());
    let mut rig = Rig::with("start-disabled", &d, |_| {});
    let before = rig.raw();
    let log = journal(&[]);
    let out = rig.startup(&log);
    assert_eq!(rig.bench.tells, NO_WRITES, "nothing is written to the CPU");
    assert!(log.asked.borrow().is_empty());
    assert_eq!(rig.raw(), before);
    assert_eq!((out.note, out.at_startup), (None, None));
    assert!(out.profile.expect("the disabled profile is reported").disabled());
}

#[test]
fn startup_does_nothing_when_the_profile_already_stands() {
    // Profile.Startup: InEffect -> return: no log, no counting, no write (inventory 1.6 step 1). Unknown cores do not
    // count (Tuning.cs Same).
    let mut deaf = measured_current();
    deaf[11] = 40;
    for (tag, ratios, deaf_core) in [("effect", measured_current(), None), ("effect-deaf", deaf, Some(11))] {
        let mut rig = Rig::with(&format!("start-{tag}"), &draft(profile::TRIAL, &ratios), |b| {
            if let Some(core) = deaf_core {
                b.deaf[core] = true;
            }
        });
        let before = rig.raw();
        let log = journal(&[event(EVENT_LOG, 6006, 4, at("13:00"))]);
        let out = rig.startup(&log);
        assert!(rig.bench.tells.is_empty(), "{tag}");
        assert!(log.asked.borrow().is_empty(), "{tag}: the log is not read");
        assert_eq!(rig.raw(), before, "{tag}: nothing is written");
        assert_eq!((out.note, out.at_startup), (None, None), "{tag}");
        assert_eq!(
            out.profile.expect("the profile is reported").boots,
            0,
            "{tag}: not counted"
        );
    }
}

#[test]
fn a_failure_after_a_risky_profile_disables_it_without_applying() {
    // Profile.Startup: failure and Risky -> State = failed, Failure = "<event> dd.MM HH:mm — после применения профиля
    // dd.MM HH:mm", Write(), return.
    let mut rig = Rig::with("start-failed", &draft(profile::TRIAL, &risky()), |_| {});
    let out = rig.startup(&journal(&[event(KERNEL_POWER, 41, 1, at("12:30"))]));
    let p = out.profile.expect("the profile is reported");
    assert_eq!(p.state.as_deref(), Some(profile::FAILED));
    assert_eq!(
        p.failure.as_deref(),
        Some("аварийное выключение или зависание 14.10 12:30 — после применения профиля 14.10 10:00")
    );
    assert!(
        rig.bench.tells.is_empty(),
        "a risky profile is not applied after a failure"
    );
    assert_eq!(out.at_startup, None);
    let disk = rig.on_disk();
    assert_eq!(disk, p, "the disabled state is written");
    assert_eq!(
        disk.applied,
        applied_at(),
        "applied keeps the time of the application that failed"
    );
    assert_eq!(disk.boots, 0);
}

#[test]
fn a_failure_after_a_safe_profile_is_noted_and_the_profile_applied() {
    // Profile.Startup: failure and not Risky -> Note "после прошлого применения был сбой (…); потолки в профиле не
    // выше заводских, поэтому он применён", then applied.
    let mut rig = Rig::with("start-safe-failure", &draft(profile::TRUSTED, &SAFE), |_| {});
    let out = rig.startup(&journal(&[event(WER, 1001, 2, at("12:30"))]));
    assert_eq!(
        out.note.as_deref(),
        Some(
            "после прошлого применения был сбой (синий экран 14.10 12:30); потолки в профиле не выше заводских, \
             поэтому он применён"
        )
    );
    assert_eq!(rig.bench.current_i32(), SAFE.to_vec());
    assert_eq!(out.at_startup.as_deref(), Some("09:00"));
    let disk = rig.on_disk();
    assert_eq!(disk.applied, morning());
    assert_eq!(disk.state.as_deref(), Some(profile::TRUSTED));
}

#[test]
fn every_failure_kind_counts_and_nothing_else_does() {
    // Profile.cs FailureEvents and FirstEvent names; WHEA 17 deliberately excluded; events before `applied` do not count;
    // DECISIONS #31: Kernel-Power 42 ends a trial stretch but is not a failure. Not a failure -> the profile is applied
    // as far as allowed (DECISIONS #31: its ×56 is refused, the rest stands).
    let cases = [
        (
            event(KERNEL_POWER, 41, 1, at("12:30")),
            Some("аварийное выключение или зависание"),
        ),
        (event(EVENT_LOG, 6008, 2, at("12:30")), Some("неожиданное выключение")),
        (event(WER, 1001, 2, at("12:30")), Some("синий экран")),
        (event(WHEA, 18, 2, at("12:30")), Some("неисправимая аппаратная ошибка")),
        (event(WHEA, 1, 1, at("12:30")), Some("неисправимая аппаратная ошибка")),
        (event(WHEA, 19, 3, at("12:30")), Some("исправленная ошибка процессора")),
        (event(WHEA, 17, 3, at("12:30")), None),
        (event(WHEA, 47, 3, at("12:30")), None),
        (event(KERNEL_POWER, 42, 4, at("12:30")), None),
        (event(EVENT_LOG, 6006, 4, at("12:30")), None),
        (event(KERNEL_POWER, 41, 1, at("09:00")), None),
    ];
    for (i, (e, name)) in cases.iter().enumerate() {
        let mut rig = Rig::with(&format!("start-kind-{i}"), &draft(profile::TRIAL, &risky()), |_| {});
        let out = rig.startup(&journal(&[*e]));
        let p = out.profile.expect("the profile is reported");
        if let Some(name) = name {
            let text = format!("{name} 14.10 12:30 — после применения профиля 14.10 10:00");
            assert_eq!(p.failure.as_deref(), Some(text.as_str()), "{e:?}");
            assert_eq!(p.state.as_deref(), Some(profile::FAILED), "{e:?}");
            assert!(rig.bench.tells.is_empty(), "{e:?}");
        } else {
            assert_ne!(p.state.as_deref(), Some(profile::FAILED), "{e:?} is not a failure");
            assert_eq!(
                rig.bench.current_i32(),
                risky_applied(),
                "{e:?}: the profile is applied"
            );
            assert_eq!(out.note.as_deref(), Some(RISKY_NOTE), "{e:?}");
        }
    }
}

#[test]
fn a_failure_after_a_clean_shutdown_is_not_the_profiles() {
    // Profile.Startup: a failure counts only if no 6006 came before it (same Windows session). The only note is the
    // refusal of the ×56 (DECISIONS #31), not a failure.
    let mut rig = Rig::with("start-after-6006", &draft(profile::TRIAL, &risky()), |_| {});
    let log = journal(&[
        event(EVENT_LOG, 6006, 4, at("11:00")),
        event(KERNEL_POWER, 41, 1, at("12:30")),
    ]);
    let out = rig.startup(&log);
    assert_eq!(out.note.as_deref(), Some(RISKY_NOTE));
    assert_eq!(rig.bench.current_i32(), risky_applied());
    let p = out.profile.expect("the profile is reported");
    assert_eq!(p.state.as_deref(), Some(profile::TRIAL));
    assert_eq!(p.boots, 1);
    assert!((p.hours - 1.0).abs() < 1e-9, "{}", p.hours);
    assert!(
        log.asked.borrow().iter().all(|&since| since == applied_at()),
        "every query starts at `applied`"
    );
}

#[test]
fn a_failure_after_hibernation_still_counts() {
    // DECISIONS #31 (a): Kernel-Power 42 ends a counted stretch for hours only; failures are attributed by 6006 alone.
    let mut rig = Rig::with("start-after-42", &draft(profile::TRIAL, &risky()), |_| {});
    let log = journal(&[
        event(KERNEL_POWER, 42, 4, at("11:00")),
        event(KERNEL_POWER, 41, 1, at("12:30")),
    ]);
    let p = rig.startup(&log).profile.expect("the profile is reported");
    assert_eq!(p.state.as_deref(), Some(profile::FAILED));
    assert_eq!(
        p.failure.as_deref(),
        Some("аварийное выключение или зависание 14.10 12:30 — после применения профиля 14.10 10:00")
    );
    assert_eq!(rig.bench.tells, NO_WRITES, "nothing is written to the CPU");
}

#[test]
fn an_unreadable_log_blocks_a_risky_profile_only() {
    // Profile.Startup catch: Risky -> Note "профиль не применён: не удалось прочитать журнал Windows и убедиться, что
    // сбоя не было (<reason>)", nothing written; not Risky -> applied.
    let broken = Journal {
        broken: Some("журнал недоступен".into()),
        ..Journal::default()
    };

    let mut rig = Rig::with("start-blind-risky", &draft(profile::TRIAL, &risky()), |_| {});
    let before = rig.raw();
    let out = rig.startup(&broken);
    assert_eq!(
        out.note.as_deref(),
        Some(
            "профиль не применён: не удалось прочитать журнал Windows и убедиться, что сбоя не было (журнал недоступен)"
        )
    );
    assert_eq!(rig.bench.tells, NO_WRITES, "nothing is written to the CPU");
    assert_eq!(rig.raw(), before, "nothing is written");
    assert_eq!(out.at_startup, None);

    let mut rig = Rig::with("start-blind-safe", &draft(profile::TRUSTED, &SAFE), |_| {});
    let out = rig.startup(&broken);
    assert_eq!(out.note, None);
    assert_eq!(rig.bench.current_i32(), SAFE.to_vec());
    assert_eq!(out.at_startup.as_deref(), Some("09:00"));
}

#[test]
fn trial_counting_and_the_way_to_trusted() {
    // Profile.Startup: only in "trial": Boots++; Hours += stretch end - Applied; trusted at Boots >= 1 and Hours >= 3.
    // DECISIONS #31 (a): the stretch ends at the first 6006 or Kernel-Power 42 after `applied`. DECISIONS #31 (15.10):
    // the profile's ×56 is refused, so it is applied without it, noted, and no time of application is reported.
    let shutdown = |hhmm| event(EVENT_LOG, 6006, 4, at(hhmm));
    let hibernate = |hhmm| event(KERNEL_POWER, 42, 4, at(hhmm));
    let cases = [
        (
            "6006 two hours later",
            profile::TRIAL,
            0,
            "0",
            vec![shutdown("12:00")],
            (1, 2.0, profile::TRIAL),
        ),
        (
            "exactly 3 h and 1 boot",
            profile::TRIAL,
            0,
            "0",
            vec![shutdown("13:00")],
            (1, 3.0, profile::TRUSTED),
        ),
        (
            "42 (fast startup) ends it too",
            profile::TRIAL,
            0,
            "0",
            vec![hibernate("13:30")],
            (1, 3.5, profile::TRUSTED),
        ),
        (
            "the first end counts",
            profile::TRIAL,
            0,
            "0.5",
            vec![hibernate("11:00"), shutdown("15:00")],
            (1, 1.5, profile::TRIAL),
        ),
        (
            "hours were enough, the boot completes it",
            profile::TRIAL,
            0,
            "5",
            vec![],
            (1, 5.0, profile::TRUSTED),
        ),
        (
            "no end of stretch: hours stay",
            profile::TRIAL,
            0,
            "2.9",
            vec![],
            (1, 2.9, profile::TRIAL),
        ),
        (
            "boots alone are not enough",
            profile::TRIAL,
            4,
            "1",
            vec![shutdown("11:30")],
            (5, 2.5, profile::TRIAL),
        ),
        (
            "a shutdown before `applied` is not this stretch",
            profile::TRIAL,
            0,
            "0",
            vec![shutdown("09:00")],
            (1, 0.0, profile::TRIAL),
        ),
        (
            "a trusted profile is not counted",
            profile::TRUSTED,
            3,
            "7",
            vec![shutdown("12:00")],
            (3, 7.0, profile::TRUSTED),
        ),
    ];
    for (i, (why, state, boots, hours, events, (boots_after, hours_after, state_after))) in
        cases.into_iter().enumerate()
    {
        let mut d = draft(state, &risky());
        (d.boots, d.hours) = (boots, hours);
        let mut rig = Rig::with(&format!("start-trial-{i}"), &d, |_| {});
        let out = rig.startup(&journal(&events));
        let p = out.profile.expect("the profile is reported");
        assert_eq!(p.boots, boots_after, "{why}");
        assert!((p.hours - hours_after).abs() < 1e-9, "{why}: hours {}", p.hours);
        assert_eq!(p.state.as_deref(), Some(state_after), "{why}");
        assert_eq!(p.applied, morning(), "{why}");
        assert_eq!(rig.on_disk(), p, "{why}: the counters are written");
        assert_eq!(rig.bench.current_i32(), risky_applied(), "{why}: applied");
        assert_eq!(out.note.as_deref(), Some(RISKY_NOTE), "{why}");
        assert_eq!(out.at_startup, None, "{why}");
    }
}

#[test]
fn applied_is_written_before_the_cpu_is() {
    // Profile.cs 160-163: the time stamp goes to disk before the CPU write, so a hang right after it is blamed on this
    // application, not the previous one. The profile is applied without its refused ×56 (DECISIONS #31).
    let mut rig = Rig::with("start-order", &draft(profile::TRIAL, &risky()), |_| {});
    rig.bench.peek = Some(rig.store.profile_path());
    let out = rig.startup(&journal(&[]));
    assert_eq!(out.note.as_deref(), Some(RISKY_NOTE));
    assert_eq!(rig.bench.current_i32(), risky_applied());
    let first = rig.bench.peeked.first().expect("the CPU was written");
    let seen: serde_json::Value = serde_json::from_str(first).expect("profile.json was whole at the first CPU write");
    assert_eq!(
        seen["applied"].as_str().and_then(Stamp::parse_roundtrip),
        Some(morning())
    );
    assert_eq!(seen["boots"].as_i64(), Some(1), "the boot is counted in the same write");
}

#[test]
fn a_partly_refused_profile_is_noted_and_applied_stays_written() {
    // Profile.Startup: refusal -> Note "профиль применён не полностью: <refusal>", no AtStartup; Write() both times.
    let mut rig = Rig::with("start-partial", &draft(profile::TRUSTED, &SAFE), |b| {
        b.refuse[0] = Some(17);
    });
    let out = rig.startup(&journal(&[]));
    assert_eq!(
        out.note.as_deref(),
        Some(
            "профиль применён не полностью: процессор не принял потолок 47 для ядра 0: код ответа 17, действующий \
             потолок остался 53"
        )
    );
    assert_eq!(out.at_startup, None);
    assert_eq!(
        rig.on_disk().applied,
        morning(),
        "applied was written although the CPU refused a block"
    );
    let mut expected = SAFE.to_vec();
    expected[0] = 53;
    assert_eq!(rig.bench.current_i32(), expected, "the other blocks are applied");
}

#[test]
fn a_damaged_profile_is_an_error_and_nothing_is_applied() {
    // Profile.Read: a partly read profile does not become the startup profile; Json.Spoiled keeps a .bad copy.
    let mut rig = Rig::new("start-damaged", |_| {});
    rig.put(&PROFILE_FIXTURE[..150]);
    let result = profile::startup(
        &rig.store,
        &mut rig.limits,
        &mut rig.bench,
        &rig.topo,
        &journal(&[]),
        &morning,
    );
    assert!(result.is_err(), "{result:?}");
    assert_eq!(rig.bench.tells, NO_WRITES, "nothing is written to the CPU");
    assert!(rig.dir.file("profile.json.bad").exists());
}

// ===============================================================================================================
// 5. Files
// ===============================================================================================================

#[test]
fn a_profile_written_by_csharp_reads_and_rewrites_byte_for_byte() {
    // Json.cs writer + Profile.Write field order; data\profile.json from the owner's machine (UTC-4 that day).
    let dir = Scratch::new("fixture");
    let store = store_in(&dir);
    fs::write(store.profile_path(), PROFILE_FIXTURE).expect("written");
    let p = store.read_profile().expect("reads").expect("the profile is ours");
    let stamp = Stamp::parse_roundtrip(FIXTURE_TIME).expect("the C# date parses");
    assert_eq!(p.name, "csmart2");
    assert_eq!(p.ratios, SAFE.to_vec());
    assert_eq!(p.state.as_deref(), Some(profile::TRUSTED));
    assert_eq!((p.saved, p.applied), (stamp, stamp));
    assert_eq!((p.boots, p.failure.as_deref()), (0, None));
    assert!(p.hours.abs() < 1e-12);
    assert_eq!(store.profile_text(&p), PROFILE_FIXTURE);
    store.write_profile(&p).expect("written");
    assert_eq!(
        fs::read(store.profile_path()).expect("read"),
        PROFILE_FIXTURE.as_bytes(),
        "UTF-8, no BOM, no newline"
    );
}

#[test]
fn profile_text_escapes_and_formats_like_the_csharp_writer() {
    // Json.cs: ", " and ": " separators, Quote escaping, doubles "0.###", null; Profile.Write field order.
    let dir = Scratch::new("text");
    let store = store_in(&dir);
    let saved_text = "2026-10-14T18:33:15.5000000-04:00";
    let p = Profile {
        name: "мой \"тихий\" \\ профиль\t2".into(),
        ratios: risky(),
        state: Some(profile::FAILED.into()),
        failure: Some("синий экран 14.10 12:30 — после применения профиля 14.10 10:00".into()),
        saved: Stamp::parse_roundtrip(saved_text).expect("parses"),
        applied: Stamp::parse_roundtrip(FIXTURE_TIME).expect("parses"),
        boots: 1,
        hours: 2.125,
    };
    let expected = format!(
        "{{\"cpu\": \"{CPU}\", \"name\": \"мой \\\"тихий\\\" \\\\ профиль\\t2\", \"lps\": {}, \"ratios\": {}, \
         \"state\": \"failed\", \"saved\": \"{saved_text}\", \"applied\": \"{FIXTURE_TIME}\", \"boots\": 1, \
         \"hours\": 2.125, \"failure\": \"синий экран 14.10 12:30 — после применения профиля 14.10 10:00\"}}",
        list(&hx_lps_i32()),
        list(&risky()),
    );
    assert_eq!(store.profile_text(&p), expected);
    store.write_profile(&p).expect("written");
    assert_eq!(
        store.read_profile().expect("reads").expect("ours"),
        p,
        "what is written reads back the same"
    );
}

#[test]
fn writing_a_profile_keeps_the_previous_one_as_bak() {
    // Json.WriteFile: .tmp, then File.Replace(tmp, path, path.bak).
    let dir = Scratch::new("bak");
    let store = store_in(&dir);
    fs::write(store.profile_path(), PROFILE_FIXTURE).expect("written");
    let mut p = store.read_profile().expect("reads").expect("ours");
    p.boots = 1;
    store.write_profile(&p).expect("written");
    assert_eq!(
        fs::read_to_string(store.profile_path()).expect("read"),
        store.profile_text(&p)
    );
    assert_eq!(
        fs::read_to_string(dir.file("profile.json.bak")).expect("the .bak is there"),
        PROFILE_FIXTURE
    );
    assert!(!dir.file("profile.json.tmp").exists());
}

#[test]
fn a_profile_of_another_cpu_or_layout_is_no_profile() {
    // Profile.Read: (cpu != this cpu || lps differ) -> return, no error, no .bad.
    for (tag, text) in [
        ("cpu", PROFILE_FIXTURE.replace(CPU, OTHER_CPU)),
        ("lps", PROFILE_FIXTURE.replace("\"lps\": [0, 1,", "\"lps\": [1, 0,")),
        (
            "short-lps",
            PROFILE_FIXTURE.replace(", 23], \"ratios\"", "], \"ratios\""),
        ),
    ] {
        let dir = Scratch::new(&format!("other-{tag}"));
        let store = store_in(&dir);
        fs::write(store.profile_path(), &text).expect("written");
        assert_eq!(store.read_profile().expect(tag), None, "{tag}");
        assert!(!dir.file("profile.json.bad").exists(), "{tag}");
    }
}

#[test]
fn a_profile_without_a_name_is_called_profile() {
    // Profile.Read: no "name" -> "Профиль" (profiles saved before names existed).
    let dir = Scratch::new("noname");
    let store = store_in(&dir);
    fs::write(
        store.profile_path(),
        PROFILE_FIXTURE.replace("\"name\": \"csmart2\", ", ""),
    )
    .expect("written");
    assert_eq!(store.read_profile().expect("reads").expect("ours").name, "Профиль");
}

#[test]
fn a_damaged_profile_is_an_error_and_kept_as_bad() {
    // Json.ReadFile / Profile.Read catch -> Json.Spoiled: "файл profile.json не читается (…); он сохранён рядом как
    // profile.json.bad", the original stays.
    let cases = [
        ("cut", PROFILE_FIXTURE[..150].to_string()),
        ("empty", String::new()),
        ("array", "[1, 2]".to_string()),
        (
            "no-applied",
            PROFILE_FIXTURE.replace(&format!(", \"applied\": \"{FIXTURE_TIME}\""), ""),
        ),
        ("bad-date", PROFILE_FIXTURE.replacen(FIXTURE_TIME, "вчера", 1)),
        (
            "bad-ratios",
            PROFILE_FIXTURE.replace("\"ratios\": [47,", "\"ratios\": [\"x\","),
        ),
    ];
    for (tag, text) in cases {
        let dir = Scratch::new(&format!("damaged-{tag}"));
        let store = store_in(&dir);
        fs::write(store.profile_path(), &text).expect("written");
        let error = store.read_profile().expect_err(tag);
        assert!(
            error.0.starts_with("файл profile.json не читается ("),
            "{tag}: {}",
            error.0
        );
        assert!(
            error.0.ends_with("; он сохранён рядом как profile.json.bad"),
            "{tag}: {}",
            error.0
        );
        assert_eq!(
            fs::read(dir.file("profile.json.bad")).expect(tag),
            text.as_bytes(),
            "{tag}"
        );
        assert_eq!(
            fs::read(store.profile_path()).expect(tag),
            text.as_bytes(),
            "{tag}: the original stays"
        );
    }
}

#[test]
fn sets_are_read_in_file_order() {
    // ProfileSets.Read; data\profiles.json from the owner's machine (C# writes it sorted, case-insensitive).
    let dir = Scratch::new("sets");
    let store = store_in(&dir);
    fs::write(store.sets_path(), SETS_FIXTURE).expect("written");
    let sets = store.read_sets().expect("reads");
    let names: Vec<&str> = sets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "74optima",
            "76optima",
            "80maxsmart",
            "csmart",
            "csmart2",
            "supercool",
            "xsmart"
        ]
    );
    assert_eq!(sets[4].ratios, SAFE.to_vec());
    assert_eq!(
        sets[0].ratios,
        [
            48, 48, 46, 46, 46, 46, 40, 40, 40, 40, 48, 48, 52, 46, 46, 46, 46, 46, 41, 41, 41, 41, 52, 52
        ]
    );
}

#[test]
fn sets_of_another_cpu_are_none_and_damaged_sets_are_an_error() {
    // ProfileSets.Read: another cpu -> nothing; any damage -> Spoiled with .bad, all or nothing.
    let dir = Scratch::new("sets-other");
    let store = store_in(&dir);
    assert_eq!(store.read_sets().expect("no file is not an error"), NO_SETS);
    fs::write(store.sets_path(), SETS_FIXTURE.replace(CPU, OTHER_CPU)).expect("written");
    assert_eq!(store.read_sets().expect("another cpu is not an error"), NO_SETS);
    for (tag, text) in [
        ("cut", SETS_FIXTURE[..300].to_string()),
        (
            "no-ratios",
            SETS_FIXTURE.replacen("\"ratios\": [51, 47,", "\"rates\": [51, 47,", 1),
        ),
    ] {
        let dir = Scratch::new(&format!("sets-{tag}"));
        let store = store_in(&dir);
        fs::write(store.sets_path(), &text).expect("written");
        assert!(store.read_sets().is_err(), "{tag}: half a file is not taken");
        assert_eq!(
            fs::read(dir.file("profiles.json.bad")).expect(tag),
            text.as_bytes(),
            "{tag}"
        );
    }
}

#[test]
fn save_records_what_stands_and_starts_a_trial_only_above_factory() {
    // Profile.Save: Ratios = Current, Saved = Applied = now, Boots 0, Hours 0, Failure null,
    // State = Risky ? trial : trusted, Write(). Since DECISIONS #31 the program itself never writes above fused, so a
    // ×56 can stand only if another program put it there; save still records what stands and starts a trial.
    let mut rig = Rig::new("save", |_| {});
    let p = profile::save(&rig.store, &rig.limits, "заводской", morning()).expect("saved");
    assert_eq!(p.name, "заводской");
    assert_eq!(p.ratios, measured_current());
    assert_eq!(p.state.as_deref(), Some(profile::TRUSTED));
    assert_eq!((p.saved, p.applied), (morning(), morning()));
    assert_eq!((p.boots, p.failure.as_deref()), (0, None));
    assert!(p.hours.abs() < 1e-12);
    assert_eq!(rig.on_disk(), p);

    rig.bench.current[12] = 56; // another program's write, past this library's range check
    assert_eq!(rig.limits.refresh(&mut rig.bench, &rig.topo), None);
    let p = profile::save(&rig.store, &rig.limits, "разгон", morning()).expect("saved");
    assert_eq!(p.ratios[12], 56, "what stands is recorded as it is");
    assert_eq!(p.state.as_deref(), Some(profile::TRIAL), "×56 over a fused ×54");
    assert_eq!(rig.on_disk(), p);
}

#[test]
fn save_starts_afresh_over_a_disabled_profile() {
    // Profile.Save resets Boots, Hours, Failure and the state.
    let mut d = draft(profile::FAILED, &risky());
    (d.boots, d.hours) = (2, "1.5");
    d.failure = Some("синий экран 14.10 12:30 — после применения профиля 14.10 10:00".into());
    let rig = Rig::with("save-over-failed", &d, |_| {});
    let p = profile::save(&rig.store, &rig.limits, "csmart2", morning()).expect("saved");
    assert_eq!(p.state.as_deref(), Some(profile::TRUSTED));
    assert_eq!((p.boots, p.failure.as_deref()), (0, None));
    assert!(p.hours.abs() < 1e-12);
    assert_eq!(rig.on_disk(), p);
}

#[test]
fn factory_removes_the_profile_and_its_bak() {
    // DECISIONS #31 (e): «заводские» deletes profile.json and profile.json.bak (C# left the .bak, B16).
    let dir = Scratch::new("forget");
    let store = store_in(&dir);
    fs::write(store.profile_path(), PROFILE_FIXTURE).expect("written");
    fs::write(dir.file("profile.json.bak"), PROFILE_FIXTURE).expect("written");
    store.forget_profile().expect("deleted");
    assert!(!store.profile_path().exists());
    assert!(!dir.file("profile.json.bak").exists());
    store.forget_profile().expect("nothing to delete is not an error");
}

#[test]
fn strings_are_quoted_like_json_quote() {
    // Json.Quote: \" and \\, \n \r \t as escapes, other chars below space as \u00xx in lower-case hex, the rest as is.
    assert_eq!(files::quote(""), "\"\"");
    assert_eq!(files::quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    assert_eq!(files::quote("1\n2\r3\t4"), "\"1\\n2\\r3\\t4\"");
    assert_eq!(
        files::quote("\u{1}\u{8}\u{c}\u{1b}\u{1f}"),
        "\"\\u0001\\u0008\\u000c\\u001b\\u001f\""
    );
    assert_eq!(
        files::quote("/ «ж» — 2–5 ×46 \u{7f} 😀"),
        "\"/ «ж» — 2–5 ×46 \u{7f} 😀\""
    );
}

#[test]
fn numbers_are_written_like_dotnet_0_hash() {
    // Json.cs: d.ToString("0.###", InvariantCulture), NaN as null; values measured on .NET Framework 4.8 here.
    for (value, text) in [
        (0.0, "0"),
        (3.0, "3"),
        (2.5, "2.5"),
        (2.125, "2.125"),
        (1.234_56, "1.235"),
        (0.0004, "0"),
        (12_345.678_9, "12345.679"),
        (1e-10, "0"),
        (1_234_567.0, "1234567"),
        (0.1 + 0.2, "0.3"),
    ] {
        assert_eq!(files::number(value), text, "{value}");
    }
    assert_eq!(files::number(f64::NAN), "null");
}

#[test]
fn number_ties_round_like_dotnet_framework() {
    // Measured on .NET Framework 4.8 (CLR 4.0.30319.42000), the C# writer's runtime: it rounds the 15-digit decimal
    // form half away from zero, so binary values just below a tie still round up.
    let measured = [(1.2345, "1.235"), (1.0005, "1.001"), (4.0045, "4.005"), (2.9995, "3")];
    let written: Vec<(f64, String)> = measured
        .iter()
        .map(|&(value, _)| (value, files::number(value)))
        .collect();
    let expected: Vec<(f64, String)> = measured
        .iter()
        .map(|&(value, text)| (value, text.to_string()))
        .collect();
    assert_eq!(
        written, expected,
        "left: what is written, right: what the C# writer writes"
    );
}

#[test]
fn integer_lists_are_written_with_comma_space() {
    // Json.cs: arrays as "[a, b]".
    assert_eq!(files::ints(&[47, 45, 36]), "[47, 45, 36]");
    assert_eq!(files::ints::<i32>(&[]), "[]");
    assert_eq!(files::ints(&hx_lps()), list(&hx_lps_i32()));
}

#[test]
fn times_use_the_csharp_round_trip_and_query_formats() {
    // Profile.Write: DateTime.ToString("o") of local time (7 digits, offset); FirstEvent: UTC "yyyy-MM-ddTHH:mm:ss.fffZ"
    // (fff truncates - measured: .9999999 -> .999); texts: "dd.MM HH:mm", "HH:mm".
    let stamp = Stamp::parse_roundtrip(FIXTURE_TIME).expect("parses");
    assert_eq!(Some(stamp), Stamp::parse_roundtrip("2026-10-14T22:33:26.1031323Z"));
    assert_eq!(Some(stamp), Stamp::parse_roundtrip("2026-10-15T00:33:26.1031323+02:00"));
    assert_eq!(stamp.utc_query(), "2026-10-14T22:33:26.103Z");
    assert_eq!(
        local("2026-10-14T22:33:26.9999999Z").utc_query(),
        "2026-10-14T22:33:26.999Z"
    );
    assert_eq!(
        stamp.roundtrip(),
        FIXTURE_TIME,
        "the owner's machine is at UTC-4 on that date"
    );
    let ten = applied_at();
    assert_eq!(ten.local(Pattern::DayTime), "14.10 10:00");
    assert_eq!(ten.local(Pattern::Time), "10:00");
    assert_eq!(ten.local(Pattern::Log), "2026-10-14 10:00:00");
    assert!(
        ten.roundtrip().starts_with("2026-10-14T10:00:00.0000000"),
        "{}",
        ten.roundtrip()
    );
    assert!((Stamp(ten.0 + 3 * HOUR).hours_since(ten) - 3.0).abs() < 1e-12);
    assert_eq!(Stamp::parse_roundtrip("вчера"), None);
}

// ===============================================================================================================
// 6. Marker of a broken measurement (LimitHold.Recover)
// ===============================================================================================================

#[test]
fn marker_restores_factory_range_ceilings_by_block_and_lists_the_rest() {
    // LimitHold.Recover: by each block's first core; skip unknown, <= 0 (null) and equal; above fused -> listed,
    // not written; message "Блокам возвращены потолки, стоявшие до него: … Не возвращены потолки выше заводского
    // предела — поставь их сам: …"; the marker and its .bak are deleted.
    let mut rig = Rig::new("marker", |_| {});
    let mut before = as_marker(&measured_current());
    before[0] = Some(50);
    before[1] = None;
    before[2..=5].copy_from_slice(&[Some(40), Some(41), Some(42), Some(43)]);
    before[10] = Some(0);
    before[12] = Some(55);
    before[23] = Some(54);
    fs::write(rig.marker(), marker_text(CPU, &hx_lps_i32(), &before)).expect("written");
    fs::write(rig.dir.file("limits-held.json.bak"), "{}").expect("written");
    let recovery = rig.recover();
    assert_eq!(
        recovery.said.as_deref(),
        Some(
            "Прошлый запуск оборвался посреди измерения. Блокам возвращены потолки, стоявшие до него: 0 ×50, 2–5 ×40, \
             23 ×54. Не возвращены потолки выше заводского предела — поставь их сам: 12 ×55."
        )
    );
    let mut expected = measured_current();
    expected[0] = 50;
    expected[2..=5].fill(40);
    expected[23] = 54;
    assert_eq!(rig.bench.current_i32(), expected);
    assert_eq!(rig.bench.tells.len(), 3, "{:?}", rig.bench.tells);
    assert!(!rig.marker().exists());
    assert!(!rig.dir.file("limits-held.json.bak").exists());
}

#[test]
fn marker_with_only_above_factory_values_writes_nothing() {
    // LimitHold.Recover: before > Fused -> "Не возвращены потолки выше заводского предела — поставь их сам: …".
    let mut rig = Rig::new("marker-above", |_| {});
    let mut before = as_marker(&measured_current());
    before[12] = Some(56);
    fs::write(rig.marker(), marker_text(CPU, &hx_lps_i32(), &before)).expect("written");
    assert_eq!(
        rig.recover().said.as_deref(),
        Some(
            "Прошлый запуск оборвался посреди измерения. Не возвращены потолки выше заводского предела — поставь их сам: 12 ×56."
        )
    );
    assert_eq!(rig.bench.tells, NO_WRITES, "nothing is written to the CPU");
    assert!(!rig.marker().exists());
}

#[test]
fn marker_reports_a_refused_restore_and_is_still_deleted() {
    // LimitHold.Recover: "Отказ: <refusal>."; finally Erase() (inventory 1.5, B2 kept on purpose).
    let mut rig = Rig::new("marker-refused", |b| b.refuse[0] = Some(17));
    let mut before = as_marker(&measured_current());
    before[0] = Some(50);
    fs::write(rig.marker(), marker_text(CPU, &hx_lps_i32(), &before)).expect("written");
    assert_eq!(
        rig.recover().said.as_deref(),
        Some(
            "Прошлый запуск оборвался посреди измерения. Отказ: процессор не принял потолок 50 для ядра 0: код ответа \
             17, действующий потолок остался 53."
        )
    );
    assert!(!rig.marker().exists());
}

#[test]
fn marker_of_another_cpu_or_matching_ceilings_says_nothing_and_is_deleted() {
    // LimitHold.Recover: another cpu/lps -> null; nothing differs -> null; Erase() in every case.
    let mut moved = as_marker(&measured_current());
    moved[0] = Some(50);
    let lps = hx_lps_i32();
    for (tag, text) in [
        ("cpu", marker_text(OTHER_CPU, &lps, &moved)),
        ("lps", marker_text(CPU, &lps[..23], &moved[..23])),
        ("same", marker_text(CPU, &lps, &as_marker(&measured_current()))),
    ] {
        let mut rig = Rig::new(&format!("marker-{tag}"), |_| {});
        fs::write(rig.marker(), text).expect("written");
        assert_eq!(rig.recover().said, None, "{tag}");
        assert!(rig.bench.tells.is_empty(), "{tag}");
        assert!(!rig.marker().exists(), "{tag}: the marker is deleted in every case");
    }
}

#[test]
fn marker_that_does_not_read_is_reported_and_deleted() {
    // LimitHold.Recover catch: "…Файл с потолками, стоявшими до него, не прочитан — проверь потолки блоков.", logged.
    for (tag, text) in [
        ("cut", "{\"cpu\": \"Intel(R) Core(TM) Ul".to_string()),
        (
            "no-limits",
            format!(
                "{{\"cpu\": \"{CPU}\", \"time\": \"2026-10-14 10:00:00\", \"lps\": {}}}",
                list(&hx_lps_i32())
            ),
        ),
    ] {
        let mut rig = Rig::new(&format!("marker-{tag}"), |_| {});
        fs::write(rig.marker(), text).expect("written");
        let recovery = rig.recover();
        assert_eq!(
            recovery.said.as_deref(),
            Some(
                "Прошлый запуск оборвался посреди измерения. Файл с потолками, стоявшими до него, не прочитан — \
                 проверь потолки блоков."
            ),
            "{tag}"
        );
        assert!(recovery.logged.is_some(), "{tag}: the cause goes to the program's log");
        assert!(rig.bench.tells.is_empty(), "{tag}");
        assert!(!rig.marker().exists(), "{tag}");
    }
}

#[test]
fn no_marker_no_recovery_and_unknown_cores_are_skipped() {
    // LimitHold.Recover: no file -> null; !Known(index) -> skipped.
    let mut rig = Rig::new("marker-none", |_| {});
    assert_eq!(rig.recover(), Recovery::default());

    let mut rig = Rig::new("marker-deaf", |b| b.deaf[11] = true);
    let mut before = as_marker(&measured_current());
    before[11] = Some(40);
    fs::write(rig.marker(), marker_text(CPU, &hx_lps_i32(), &before)).expect("written");
    assert_eq!(rig.recover().said, None);
    assert_eq!(rig.bench.tells, NO_WRITES, "nothing is written to the CPU");
}

// ===============================================================================================================
// 7. Which saved set stands (LimitsStartup.Named)
// ===============================================================================================================

fn set(name: &str, ratios: &[i32]) -> Set {
    Set {
        name: name.into(),
        ratios: ratios.to_vec(),
    }
}

fn startup_profile(name: &str, ratios: &[i32]) -> Profile {
    Profile {
        name: name.into(),
        ratios: ratios.to_vec(),
        state: Some(profile::TRUSTED.into()),
        failure: None,
        saved: saved_at(),
        applied: applied_at(),
        boots: 0,
        hours: 0.0,
    }
}

#[test]
fn standing_is_factory_first_when_there_is_no_startup_profile() {
    // Named: !profile.Exists && same(Fused) -> "".
    let limits = limits_with(at_factory);
    assert_eq!(
        profile::standing(&limits, None, &[set("stock", &measured_fused())]),
        Some(String::new())
    );
}

#[test]
fn standing_with_a_startup_profile_checks_sets_before_factory() {
    // Named: the startup profile, then sets in order, and factory ("") last.
    let limits = limits_with(at_factory);
    let b = startup_profile("B", &SAFE);
    let sets = [set("A", &measured_fused()), set("B", &SAFE)];
    assert_eq!(profile::standing(&limits, Some(&b), &sets), Some("A".into()));
    assert_eq!(
        profile::standing(&limits, Some(&b), &[set("B", &SAFE)]),
        Some(String::new())
    );
}

#[test]
fn standing_names_the_startup_profile_before_other_sets() {
    // Named: profile.Name first when its ratios stand, then the sets in their order.
    let limits = limits_with(at_safe);
    let b = startup_profile("B", &SAFE);
    let sets = [set("A", &SAFE), set("B", &SAFE), set("C", &SAFE)];
    assert_eq!(profile::standing(&limits, Some(&b), &sets), Some("B".into()));
    assert_eq!(
        profile::standing(&limits, None, &[set("A", &SAFE), set("C", &SAFE)]),
        Some("A".into())
    );
}

#[test]
fn standing_is_none_for_ceilings_set_by_hand() {
    // Named: no set and not factory -> null.
    let limits = limits_with(|b| {
        at_safe(b);
        b.current[0] = 40;
    });
    let b = startup_profile("B", &SAFE);
    assert_eq!(profile::standing(&limits, Some(&b), &[set("B", &SAFE)]), None);
}

#[test]
fn standing_ignores_cores_that_never_answered() {
    // Named: same() skips !limits.Known(i).
    let mut other = SAFE.to_vec();
    other[11] = 33;
    let limits = limits_with(|b| {
        at_safe(b);
        b.deaf[11] = true;
    });
    assert_eq!(profile::standing(&limits, None, &[set("A", &other)]), Some("A".into()));
    let limits = limits_with(|b| {
        at_factory(b);
        b.deaf[11] = true;
    });
    assert_eq!(profile::standing(&limits, None, &[]), Some(String::new()));
}

#[test]
fn standing_finds_the_startup_profile_only_through_the_list() {
    // Named: profiles.Get(profile.Name) != null is required; Get compares names ignoring case.
    let limits = limits_with(at_safe);
    let b = startup_profile("B", &SAFE);
    assert_eq!(profile::standing(&limits, Some(&b), &[]), None);
    assert_eq!(
        profile::standing(&limits, Some(&b), &[set("b", &SAFE)]),
        Some("B".into())
    );
    assert!(profile::same_name("CSmart2 ", "csmart2"));
    assert!(!profile::same_name("csmart", "csmart2"));
}
