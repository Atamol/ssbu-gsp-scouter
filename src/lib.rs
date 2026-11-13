use std::ffi::{c_char, CStr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use skyline::hooks::{getRegionAddress, InlineCtx, Region};

const OFF_INFO: usize = 0x52c5180;
// The battle's copy of the mode, the results screen only converts ratings in 0x0e and 0x10
const OFF_MODE: usize = 0x52cc5d8;
const OFF_RANK_ROOT: usize = 0x5314510;
const OFF_ONLINE_ROOT: usize = 0x52c4c58;
const OFF_RATING_TO_GSP: usize = 0x17d1480;
const OFF_SET_TEXT: usize = 0x37a2270;
const OFF_FIND_PANE: usize = 0x3776910;
const OFF_RELEASE_PANE: usize = 0x392eb40;

// first instruction after the record fill loop of 0x169bef0
const OFF_RECORDS_FILLED: usize = 0x169e74c;
const OFF_NAME_PLATE: usize = 0x141a9b0;
const OFF_VS_PANELS: usize = 0x1ee9990;
const OFF_HUD_UPDATE: usize = 0x139d870;
const OFF_MATCH_START: usize = 0x156c0b4;
const OFF_SUDDEN_DEATH_START: usize = 0x158b784;

const REC: usize = 0x4ed88;
const REC_STRIDE: usize = 0xb0;
const REC_RATING: usize = 0x70;
const PRESENT_MASK: usize = 0x4f314;
const SLOTS: usize = 0x5b0;
const SLOT_STRIDE: usize = 0x260;

// The rank table index the results screen passes
const GSP_TABLE: u32 = 0x15;
const VS_ENTRY: usize = 0xd8;
const HUD_ENTRY: usize = 0x2d0;
const HUD_BLOCK: usize = 0xd90;

const GUARDS: &[(usize, &[u32])] = &[
    (0x1d5ac8c, &[0xd001ab96, 0x395762c8]),
    (0x1d5aca4, &[0xd001adc8, 0xf9428908]),
    (0x1d5acd0, &[0x529dbf09, 0x72a00089, 0xb8696902]),
    (0x1d5adc8, &[0xf94002c0, 0x528002a1, 0x97e9d9ac]),
    (0x169de64, &[0x529e6289, 0x72a00089]),
    (0x169e740, &[0x54ffb901, 0xd001e3a8, 0x91126108, 0x12800009, 0xb9000509]),
    (0x172adb0, &[0xd001dcc8, 0xf9462d08]),
    (0x141abbc, &[0xf001f54a, 0x9106014a]),
    (0x141ac10, &[0xf94042a8, 0x5281b209]),
    (0x141ac34, &[0x948e1d8f]),
    (0x1ee9c68, &[0xa94f231b]),
    (0x1ee9d0c, &[0xb9800368]),
    (0x1ee9ee4, &[0x91028368]),
    (0x1eeaba4, &[0x94622f5b]),
    (0x1eeabc0, &[0x9462ddac]),
    (0x1eead60, &[0x94690f78]),
    (0x1eeb754, &[0x9103637b]),
    (0x139d898, &[0xf940281b]),
    (0x139d964, &[0x9102837c]),
    (0x139df94, &[0x39467388]),
    (0x139df9c, &[0x3906739f]),
    (0x139e074, &[0xb9800388]),
    (0x139e08c, &[0x911d8100]),
    (0x139e0cc, &[0x910b439c]),
    (0x156c0b4, &[0xf9400308]),
    (0x158b784, &[0xf9400a68]),
];

// The records keep the last match until the game fills them again, so a fill only counts for
// the next VS screen and the match after it. A match dropped before READY GO would otherwise
// leave it set for the next opponent
static FILLED: AtomicBool = AtomicBool::new(false);
static VS_FRESH: AtomicBool = AtomicBool::new(false);
static VS_THIS: AtomicUsize = AtomicUsize::new(0);
static MATCH_FRESH: AtomicBool = AtomicBool::new(false);
static START: AtomicBool = AtomicBool::new(false);
static VS_EARLY_LOGGED: AtomicBool = AtomicBool::new(false);

// socket-sniffer looks this up and leaves the names to this plugin. Data rather than an empty
// function, those get folded into other symbols
#[allow(non_upper_case_globals)]
#[no_mangle]
#[used]
pub static ssbu_gsp_scouter_version: u32 = 0x0001_0000;

#[repr(C)]
struct PaneRef {
    vtable: usize,
    holder: usize,
    // A 16-byte return would come back in x0/x1, the callee writes through x8
    _sret: usize,
}

fn text() -> usize {
    unsafe { getRegionAddress(Region::Text) as usize }
}

fn info() -> usize {
    text() + OFF_INFO
}

unsafe fn rd<T: Copy>(addr: usize) -> T {
    *(addr as *const T)
}

unsafe fn ptr(addr: usize) -> Option<usize> {
    let p = rd::<usize>(addr);
    (p != 0).then_some(p)
}

unsafe fn ranked() -> bool {
    matches!(rd::<u8>(text() + OFF_MODE), 0x0e | 0x10)
}

unsafe fn duel() -> Option<[usize; 2]> {
    let mask = rd::<u32>(info() + PRESENT_MASK) & 0xff;
    if mask.count_ones() != 2 {
        return None;
    }
    let a = mask.trailing_zeros() as usize;
    let b = (mask & !(1 << a)).trailing_zeros() as usize;
    Some([a, b])
}

// The results screen converts every present record, whatever the player count
unsafe fn present(p: usize) -> bool {
    let mask = rd::<u32>(info() + PRESENT_MASK) & 0xff;
    mask.count_ones() >= 2 && mask & (1 << p) != 0
}

fn record(p: usize) -> usize {
    info() + REC + p * REC_STRIDE
}

unsafe fn rating(p: usize) -> u32 {
    rd::<u32>(record(p) + REC_RATING)
}

unsafe fn filled(p: usize) -> bool {
    rd::<u8>(record(p)) != 0xff
}

fn fresh() -> bool {
    FILLED.load(Ordering::Relaxed) || VS_FRESH.load(Ordering::Relaxed) || MATCH_FRESH.load(Ordering::Relaxed)
}

unsafe fn player_of_slot(slot: i32) -> Option<usize> {
    (0..8).map(|k| info() + SLOTS + k * SLOT_STRIDE)
        .find(|e| rd::<i32>(e + 0xc) == slot)
        .map(|e| rd::<i32>(e + 0x14))
        .filter(|p| (0..8).contains(p))
        .map(|p| p as usize)
}

unsafe fn slot_of_player(p: usize) -> Option<i32> {
    (0..8).map(|k| info() + SLOTS + k * SLOT_STRIDE)
        .find(|e| rd::<i32>(e + 0x14) == p as i32)
        .map(|e| rd::<i32>(e + 0xc))
        .filter(|s| (0..8).contains(s))
}

unsafe fn rank_table() -> Option<usize> {
    let root = ptr(ptr(text() + OFF_RANK_ROOT)?)?;
    let table = ptr(ptr(root + 0x68)?)?;
    ptr(table + 8)?;
    Some(table)
}

// 0x17d1480 walks the downloaded server settings without null checks
unsafe fn settings_loaded() -> bool {
    let cfg = ptr(text() + OFF_ONLINE_ROOT)
        .and_then(|root| ptr(root + 8))
        .and_then(|online| ptr(online + 0xb0));
    cfg.map_or(false, |cfg| ptr(cfg + 8).is_some() && ptr(cfg + 0x10).is_some())
}

unsafe fn to_gsp(raw: u32) -> Option<u32> {
    if raw == 0 || !settings_loaded() {
        return None;
    }
    let table = rank_table()?;
    let convert: extern "C" fn(usize, u32, u32) -> u32 = std::mem::transmute(text() + OFF_RATING_TO_GSP);
    let gsp = convert(table, GSP_TABLE, raw);
    // 0 and 1 come back for ratings outside the table
    (gsp >= 2).then_some(gsp)
}

fn format(gsp: u32) -> String {
    let digits = gsp.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn utf16z(s: &str) -> [u16; 24] {
    let mut buf = [0u16; 24];
    for (dst, unit) in buf.iter_mut().take(23).zip(s.encode_utf16()) {
        *dst = unit;
    }
    buf
}

unsafe fn set_text(pane: usize, text_utf16: &[u16]) {
    let set: extern "C" fn(usize, *const u16, i32, u32) = std::mem::transmute(text() + OFF_SET_TEXT);
    set(pane, text_utf16.as_ptr(), -1, 1);
}

// Children hang off an intrusive list at +0x20, linked through each child's +0x8
unsafe fn find_pane(pane: usize, name: &CStr, depth: u32) -> Option<usize> {
    if depth > 16 {
        return None;
    }
    let want = name.to_bytes_with_nul();
    if std::slice::from_raw_parts((pane + 0xb0) as *const u8, want.len()) == want {
        return Some(pane);
    }
    let head = pane + 0x20;
    let mut node = rd::<usize>(pane + 0x28);
    let mut n = 0;
    while node != 0 && node != head && n < 512 {
        if let Some(found) = find_pane(node - 8, name, depth + 1) {
            return Some(found);
        }
        node = rd::<usize>(node + 8);
        n += 1;
    }
    None
}

#[skyline::hook(offset = OFF_RECORDS_FILLED, inline)]
unsafe fn hook_records_filled(_ctx: &InlineCtx) {
    FILLED.store(true, Ordering::Relaxed);
}

#[skyline::hook(offset = OFF_NAME_PLATE)]
unsafe fn hook_name_plate(this: usize, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64, a7: u64) -> u64 {
    let ret = call_original!(this, a1, a2, a3, a4, a5, a6, a7);
    name_plate(this);
    ret
}

unsafe fn name_plate(this: usize) {
    if !ranked() || !fresh() {
        return;
    }
    let slot = rd::<i32>(this + 0xd8);
    let Some(p) = player_of_slot(slot) else { return };
    if !present(p) || !filled(p) {
        return;
    }
    let Some(gsp) = to_gsp(rating(p)) else { return };
    let label = utf16z(&format(gsp));
    let Some(blocks) = ptr(this + 0xc0).and_then(|owner| ptr(owner + 0x80)) else { return };
    let block = blocks + slot as usize * HUD_BLOCK;
    for off in [0x300, 0xf38] {
        if let Some(pane) = ptr(block + off).and_then(|p| ptr(p + 0x10)) {
            set_text(pane, &label);
        }
    }
}

#[skyline::hook(offset = OFF_VS_PANELS)]
unsafe fn hook_vs_panels(this: usize, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64, a7: u64) -> u64 {
    let ret = call_original!(this, a1, a2, a3, a4, a5, a6, a7);
    vs_panels(this);
    ret
}

unsafe fn vs_panels(this: usize) {
    // Decided once per VS screen, the function may run again for the same one
    if VS_THIS.swap(this, Ordering::Relaxed) != this {
        VS_FRESH.store(FILLED.swap(false, Ordering::Relaxed), Ordering::Relaxed);
        MATCH_FRESH.store(false, Ordering::Relaxed);
    }
    if !ranked() {
        return;
    }
    if !VS_FRESH.load(Ordering::Relaxed) {
        if !VS_EARLY_LOGGED.swap(true, Ordering::Relaxed) {
            println!("[gsp-scouter] VS screen came before the records were filled, names left as is");
        }
        return;
    }
    let (begin, end) = (rd::<usize>(this + 0xf0), rd::<usize>(this + 0xf8));
    let size = end.wrapping_sub(begin);
    if begin == 0 || size % VS_ENTRY != 0 || !(2..=8).contains(&(size / VS_ENTRY)) {
        return;
    }
    let find: extern "C" fn(usize, *const c_char) -> PaneRef = std::mem::transmute(text() + OFF_FIND_PANE);
    let release: extern "C" fn(usize) = std::mem::transmute(text() + OFF_RELEASE_PANE);
    for i in 0..size / VS_ENTRY {
        let entry = begin + i * VS_ENTRY;
        let row = rd::<i32>(entry);
        if !(0..8).contains(&row) {
            continue;
        }
        let p = rd::<i32>(info() + SLOTS + row as usize * SLOT_STRIDE + 0x14);
        if !(0..8).contains(&p) || !filled(p as usize) {
            continue;
        }
        let Some(gsp) = to_gsp(rating(p as usize)) else { continue };
        let label = utf16z(&format(gsp));
        for name in [c"set_txt_fighter_1", c"set_txt_fighter_2"] {
            let pane = find(entry + 0xa0, name.as_ptr());
            if pane.holder == 0 {
                continue;
            }
            if let Some(textbox) = ptr(pane.holder + 0x10) {
                set_text(textbox, &label);
            }
            release(pane.holder);
        }
    }
}

#[skyline::hook(offset = OFF_MATCH_START, inline)]
unsafe fn hook_match_start(_ctx: &InlineCtx) {
    let fresh = VS_FRESH.swap(false, Ordering::Relaxed) | FILLED.swap(false, Ordering::Relaxed);
    MATCH_FRESH.store(fresh, Ordering::Relaxed);
    VS_THIS.store(0, Ordering::Relaxed);
    VS_EARLY_LOGGED.store(false, Ordering::Relaxed);
    START.store(true, Ordering::Relaxed);
}

#[skyline::hook(offset = OFF_SUDDEN_DEATH_START, inline)]
unsafe fn hook_sudden_death_start(_ctx: &InlineCtx) {
    START.store(true, Ordering::Relaxed);
}

#[skyline::hook(offset = OFF_HUD_UPDATE)]
unsafe fn hook_hud_update(this: usize, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64, a7: u64) -> u64 {
    if START.swap(false, Ordering::Relaxed) {
        higher_gsp(this);
    }
    call_original!(this, a1, a2, a3, a4, a5, a6, a7)
}

// The HUD update right after sets off the game's own reach popup for the entry with +0x19c set
unsafe fn higher_gsp(this: usize) {
    if !ranked() || !MATCH_FRESH.load(Ordering::Relaxed) {
        return;
    }
    let Some(hud) = ptr(this + 0x50) else { return };
    let Some([a, b]) = duel() else { return };
    let (Some(ga), Some(gb)) = (to_gsp(rating(a)), to_gsp(rating(b))) else { return };
    if ga == gb {
        return;
    }
    let Some(slot) = slot_of_player(if ga > gb { a } else { b }) else { return };
    let Some(entry) = (0..8).map(|j| hud + 0xa0 + j * HUD_ENTRY).find(|e| rd::<i32>(*e) == slot) else { return };
    let Some(pane) = ptr(hud + 0x10)
        .and_then(|layout| ptr(layout + 0x80))
        .and_then(|blocks| ptr(blocks + slot as usize * HUD_BLOCK + 0x300))
        .and_then(|plate| ptr(plate + 0x10))
        .and_then(|textbox| ptr(textbox + 0x18))
        .and_then(|parent| find_pane(parent, c"txt_reach", 0))
    else {
        return;
    };
    set_text(pane, &utf16z(&format!("+{} GSP", format(ga.abs_diff(gb)))));
    *((entry + 0x19c) as *mut u8) = 1;
}

unsafe fn version_matches() -> bool {
    GUARDS.iter().all(|(off, words)| {
        words.iter().enumerate().all(|(i, w)| rd::<u32>(text() + off + i * 4) == *w)
    })
}

fn gsp_lens_loaded() -> bool {
    let mut addr = 0usize;
    unsafe {
        skyline::nn::ro::LookupSymbol(&mut addr, c"gsp_lens_set_name".as_ptr() as *const u8) == 0 && addr != 0
    }
}

#[skyline::main(name = "ssbu_gsp_scouter")]
pub fn main() {
    if !unsafe { version_matches() } {
        println!("[gsp-scouter] code doesn't match 13.0.5, off");
        return;
    }
    if gsp_lens_loaded() {
        println!("[gsp-scouter] gsp-lens is loaded, leaving the names to it");
        return;
    }
    skyline::install_hooks!(
        hook_records_filled, hook_name_plate, hook_vs_panels, hook_hud_update,
        hook_match_start, hook_sudden_death_start,
    );
    println!("[gsp-scouter] ready");
}
