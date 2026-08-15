//! Darask Paint プロジェクト形式 (`.dpaint`) v1。
//!
//! 現在のレイヤー状態に加えて undo/redo の全エントリとカーソルを保存する。
//! 画素は 256×256 以下のタイルへ分け、64-bit content hash と bytes 比較で
//! 全履歴横断の重複排除を行う。コンテナは長さ付き chunk + CRC32 なので、
//! 将来の未知 chunk を安全に読み飛ばせる。

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::document::{next_layer_uid, DocSnapshot, Document, IRect, Layer, MAX_LAYERS};
use crate::history::{History, HistoryEntry, HistoryOp, PatchRegion};

const MAGIC: &[u8; 8] = b"DPAINT\x1a\0";
/// v10 §47: v2 = AddLayer に visible/opacity、MergeDown に結合結果メタ
/// (`merged_*`)を追加した符号化。読み込みは v1(v0.7〜v0.9)も受け付ける
/// (`parse_chunks`/`decode_op` の分岐参照)。
///
/// v12 §50.4: v3 = Layer レコードの `reserved u16`(v1/v2 では 0 必須)を
/// `blend: u8` + `flags: u8`(bit0 = alpha_lock)として**同じ 2 バイトのまま
/// 再解釈**し、AddLayer/MergeDown の redo 用メタにも同じ 2 バイトを足した
/// 符号化。後続フィールドのオフセットは v2 から変わらない。書き込みは常に
/// v3、読み込みは v1/v2/v3 のすべてに対応する。
const VERSION: u16 = 3;
const ENDIAN_LITTLE: u8 = 1;
const HEADER_SIZE: u8 = 16;
const TILE_SIZE: u32 = 256;
const CHECKPOINT_INTERVAL: u32 = 16;
// v8 レビュー修正: 寸法上限は `document.rs` に一本化した(「開く」/
// クリップボードの検査(io.rs)と同じ値を共有する)。
const MAX_DIMENSION: u32 = crate::document::MAX_DIMENSION;
const MAX_REVISIONS: usize = 1_000_000;
const MAX_TILES: usize = 4_000_000;
const MAX_STRING_BYTES: usize = 1024 * 1024;
const MAX_DISPLAY_STEPS: usize = 500;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 1024 * 1024 * 1024;
const MAX_PROJECT_MEMORY_BYTES: usize = 2 * 1024 * 1024 * 1024;
const PROJECT_MEMORY_SAFETY_BYTES: usize = 1024 * 1024;
const MIN_ENCODED_TILE_BYTES: usize = 13;
const MIN_ENCODED_LAYER_BYTES: usize = 32;
const MIN_ENCODED_PATCH_REGION_BYTES: usize = 24;
const MIN_ENCODED_REVISION_BYTES: usize = 48;

const CHUNK_META: [u8; 4] = *b"META";
const CHUNK_TILES: [u8; 4] = *b"TILS";
const CHUNK_DOCUMENT: [u8; 4] = *b"DOCS";
const CHUNK_REVISIONS: [u8; 4] = *b"REVS";

static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct TileStore {
    tiles: Vec<Vec<u8>>,
    by_hash: HashMap<u64, Vec<u32>>,
}

impl TileStore {
    fn intern(&mut self, bytes: &[u8]) -> Result<u32, String> {
        let hash = content_hash(bytes);
        if let Some(candidates) = self.by_hash.get(&hash) {
            for &id in candidates {
                if self
                    .tiles
                    .get(id as usize)
                    .is_some_and(|tile| tile == bytes)
                {
                    return Ok(id);
                }
            }
        }
        if self.tiles.len() >= MAX_TILES || self.tiles.len() > u32::MAX as usize {
            return Err("プロジェクトのタイル数が上限を超えました".to_owned());
        }
        let id = self.tiles.len() as u32;
        self.tiles
            .try_reserve(1)
            .map_err(|_| "保存タイル一覧を確保できません".to_owned())?;
        self.by_hash
            .try_reserve(1)
            .map_err(|_| "保存タイル索引を確保できません".to_owned())?;
        let owned = try_clone_bytes(bytes, "保存タイル")?;
        self.tiles.push(owned);
        let candidates = self.by_hash.entry(hash).or_default();
        candidates
            .try_reserve(1)
            .map_err(|_| "保存タイル候補を確保できません".to_owned())?;
        candidates.push(id);
        Ok(id)
    }
}

fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = i as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                0xedb8_8320_u32 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
}

const CRC_TABLE: [u32; 256] = crc_table();

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        let index = ((crc ^ u32::from(byte)) & 0xff) as usize;
        crc = CRC_TABLE[index] ^ (crc >> 8);
    }
    !crc
}

fn put_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_len(out: &mut Vec<u8>, value: usize, what: &str) -> Result<(), String> {
    let value = u32::try_from(value).map_err(|_| format!("{what}が長すぎます"))?;
    put_u32(out, value);
    Ok(())
}

fn put_string(out: &mut Vec<u8>, value: &str) -> Result<(), String> {
    if value.len() > MAX_STRING_BYTES {
        return Err("文字列が長すぎます".to_owned());
    }
    let additional = 4usize
        .checked_add(value.len())
        .ok_or_else(|| "文字列が長すぎます".to_owned())?;
    try_reserve_exact(out, additional, "文字列")?;
    put_len(out, value.len(), "文字列")?;
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| "プロジェクトの長さが不正です".to_owned())?;
        let value = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| "プロジェクトが途中で切れています".to_owned())?;
        self.pos = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let bytes: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| "u16を読めません".to_owned())?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| "u32を読めません".to_owned())?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn i32(&mut self) -> Result<i32, String> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| "i32を読めません".to_owned())?;
        Ok(i32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| "u64を読めません".to_owned())?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn string(&mut self, budget: &mut ProjectMemoryBudget) -> Result<String, String> {
        let len = self.u32()? as usize;
        if len > MAX_STRING_BYTES {
            return Err("プロジェクト内の文字列が長すぎます".to_owned());
        }
        let bytes = self.take(len)?;
        budget.add_heap_buffer(len)?;
        let mut owned = Vec::new();
        try_reserve_exact(&mut owned, len, "文字列")?;
        owned.extend_from_slice(bytes);
        String::from_utf8(owned).map_err(|_| "文字列がUTF-8ではありません".to_owned())
    }

    fn finish(self, what: &str) -> Result<(), String> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(format!("{what}に余分なデータがあります"))
        }
    }
}

#[derive(Default)]
struct ProjectMemoryBudget {
    bytes: usize,
}

impl ProjectMemoryBudget {
    fn with_encoded_len(encoded_len: usize) -> Result<Self, String> {
        let mut budget = Self::default();
        budget.add(encoded_len)?;
        budget.add(PROJECT_MEMORY_SAFETY_BYTES)?;
        Ok(budget)
    }

    fn add(&mut self, bytes: usize) -> Result<(), String> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| "復元サイズが大きすぎます".to_owned())?;
        if self.bytes > MAX_PROJECT_MEMORY_BYTES {
            return Err("プロジェクトの復元メモリが安全上限を超えています".to_owned());
        }
        Ok(())
    }

    fn add_vec<T>(&mut self, count: usize) -> Result<(), String> {
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| "復元サイズが大きすぎます".to_owned())?;
        self.add_heap_buffer(bytes)
    }

    fn add_heap_buffer(&mut self, bytes: usize) -> Result<(), String> {
        let aligned = bytes
            .checked_add(15)
            .map(|value| value & !15)
            .ok_or_else(|| "復元サイズが大きすぎます".to_owned())?;
        self.add(aligned)
    }
}

fn try_reserve_exact<T>(values: &mut Vec<T>, count: usize, what: &str) -> Result<(), String> {
    values
        .try_reserve_exact(count)
        .map_err(|_| format!("{what}のメモリを確保できません"))
}

fn try_clone_bytes(bytes: &[u8], what: &str) -> Result<Vec<u8>, String> {
    let mut copy = Vec::new();
    try_reserve_exact(&mut copy, bytes.len(), what)?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn require_minimum_remaining(
    reader: &Reader<'_>,
    count: usize,
    minimum_each: usize,
    what: &str,
) -> Result<(), String> {
    let minimum = count
        .checked_mul(minimum_each)
        .ok_or_else(|| format!("{what}の長さが大きすぎます"))?;
    if minimum > reader.remaining() {
        return Err(format!("{what}の件数と残り長さが一致しません"));
    }
    Ok(())
}

fn pixel_len(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err("画像寸法が対応範囲外です".to_owned());
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|count| count.checked_mul(4))
        .ok_or_else(|| "画像寸法が大きすぎます".to_owned())
}

fn tile_grid(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(TILE_SIZE), height.div_ceil(TILE_SIZE))
}

fn copy_tile(
    pixels: &[u8],
    width: u32,
    x: u32,
    y: u32,
    tile_width: u32,
    tile_height: u32,
) -> Result<Vec<u8>, String> {
    let len = pixel_len(tile_width, tile_height)?;
    let mut tile = Vec::new();
    try_reserve_exact(&mut tile, len, "保存タイル画素")?;
    let row_bytes = tile_width as usize * 4;
    for row in 0..tile_height {
        let start = ((y + row) as usize)
            .checked_mul(width as usize)
            .and_then(|value| value.checked_add(x as usize))
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| "タイル位置が大きすぎます".to_owned())?;
        let source = pixels
            .get(start..start + row_bytes)
            .ok_or_else(|| "レイヤー画素長が不正です".to_owned())?;
        tile.extend_from_slice(source);
    }
    Ok(tile)
}

/// v12 §50.4: Layer レコードの `flags: u8`(bit0 = alpha_lock、他ビットは
/// 0 必須)。`blend: u8` と合わせて v1/v2 の `reserved u16` と同じ 2 バイトを
/// 占める(後続フィールドのオフセット不変)。
const LAYER_FLAG_ALPHA_LOCK: u8 = 0b0000_0001;
/// 現時点で定義済みのビット全体(これ以外のビットが立っていれば拒否する)。
const LAYER_FLAGS_KNOWN: u8 = LAYER_FLAG_ALPHA_LOCK;

/// v12 §50.4: レイヤーメタの 2 バイト(blend + flags)を書く。v1/v2 は
/// `reserved u16 = 0` のまま(旧アプリが読める形)。
fn put_layer_meta(
    out: &mut Vec<u8>,
    blend: crate::document::BlendMode,
    alpha_lock: bool,
    version: u16,
) {
    if version >= 3 {
        put_u8(out, blend.to_u8());
        put_u8(out, if alpha_lock { LAYER_FLAG_ALPHA_LOCK } else { 0 });
    } else {
        put_u16(out, 0);
    }
}

/// v12 §50.4: レイヤーメタの 2 バイトを読む。v1/v2 は `reserved u16` が 0 で
/// あることを検証し、通常ブレンド・ロック無しで補完する(ARCHITECTURE.md
/// §22.8-8: 「非 0 拒否」は必ず version 分岐にすること)。
fn read_layer_meta(
    reader: &mut Reader<'_>,
    version: u16,
) -> Result<(crate::document::BlendMode, bool), String> {
    if version >= 3 {
        let blend = crate::document::BlendMode::from_u8(reader.u8()?)
            .ok_or_else(|| "レイヤーのブレンドモードが不正です".to_owned())?;
        let flags = reader.u8()?;
        if flags & !LAYER_FLAGS_KNOWN != 0 {
            return Err("レイヤーのフラグが不正です".to_owned());
        }
        Ok((blend, flags & LAYER_FLAG_ALPHA_LOCK != 0))
    } else {
        if reader.u16()? != 0 {
            return Err("レイヤーの予約領域が不正です".to_owned());
        }
        Ok((crate::document::BlendMode::Normal, false))
    }
}

fn encode_layer(
    out: &mut Vec<u8>,
    layer: &Layer,
    width: u32,
    height: u32,
    tiles: &mut TileStore,
    version: u16,
) -> Result<(), String> {
    if layer.pixels.len() != pixel_len(width, height)? {
        return Err("レイヤー画素長が不正です".to_owned());
    }
    let (columns, rows) = tile_grid(width, height);
    let tile_count = columns
        .checked_mul(rows)
        .ok_or_else(|| "タイル数が大きすぎます".to_owned())?;
    let tile_id_bytes = (tile_count as usize)
        .checked_mul(4)
        .ok_or_else(|| "タイル数が大きすぎます".to_owned())?;
    let encoded_len = 28usize
        .checked_add(layer.name.len())
        .and_then(|len| len.checked_add(tile_id_bytes))
        .ok_or_else(|| "レイヤー情報が大きすぎます".to_owned())?;
    try_reserve_exact(out, encoded_len, "レイヤー情報")?;
    put_u32(out, width);
    put_u32(out, height);
    put_string(out, &layer.name)?;
    put_u8(out, u8::from(layer.visible));
    put_u8(out, layer.opacity);
    put_layer_meta(out, layer.blend, layer.alpha_lock, version);
    put_u32(out, columns);
    put_u32(out, rows);
    put_u32(out, tile_count);
    for ty in 0..rows {
        for tx in 0..columns {
            let x = tx * TILE_SIZE;
            let y = ty * TILE_SIZE;
            let tile_width = TILE_SIZE.min(width - x);
            let tile_height = TILE_SIZE.min(height - y);
            let tile = copy_tile(&layer.pixels, width, x, y, tile_width, tile_height)?;
            put_u32(out, tiles.intern(&tile)?);
        }
    }
    Ok(())
}

fn encode_snapshot(
    out: &mut Vec<u8>,
    snapshot: &DocSnapshot,
    tiles: &mut TileStore,
    version: u16,
) -> Result<(), String> {
    pixel_len(snapshot.width, snapshot.height)?;
    if snapshot.layers.is_empty() || snapshot.layers.len() > MAX_LAYERS {
        return Err("レイヤー数が対応範囲外です".to_owned());
    }
    if snapshot.active >= snapshot.layers.len() {
        return Err("アクティブレイヤーが不正です".to_owned());
    }
    try_reserve_exact(out, 16, "スナップショット情報")?;
    put_u32(out, snapshot.width);
    put_u32(out, snapshot.height);
    put_len(out, snapshot.active, "アクティブレイヤー")?;
    put_len(out, snapshot.layers.len(), "レイヤー数")?;
    for layer in &snapshot.layers {
        encode_layer(out, layer, snapshot.width, snapshot.height, tiles, version)?;
    }
    Ok(())
}

fn encode_document(
    out: &mut Vec<u8>,
    doc: &Document,
    tiles: &mut TileStore,
    version: u16,
) -> Result<(), String> {
    pixel_len(doc.width, doc.height)?;
    if doc.layers.is_empty() || doc.layers.len() > MAX_LAYERS || doc.active >= doc.layers.len() {
        return Err("ドキュメントのレイヤー構造が不正です".to_owned());
    }
    try_reserve_exact(out, 16, "ドキュメント情報")?;
    put_u32(out, doc.width);
    put_u32(out, doc.height);
    put_len(out, doc.active, "アクティブレイヤー")?;
    put_len(out, doc.layers.len(), "レイヤー数")?;
    for layer in &doc.layers {
        encode_layer(out, layer, doc.width, doc.height, tiles, version)?;
    }
    Ok(())
}

fn op_kind(op: &HistoryOp) -> u8 {
    match op {
        HistoryOp::Patch { .. } => 1,
        HistoryOp::AddLayer { .. } => 2,
        HistoryOp::DuplicateLayer { .. } => 3,
        HistoryOp::RemoveLayer { .. } => 4,
        HistoryOp::MoveLayer { .. } => 5,
        HistoryOp::MergeDown { .. } => 6,
        HistoryOp::ReplaceAll { .. } => 7,
    }
}

fn encode_op(
    out: &mut Vec<u8>,
    op: &HistoryOp,
    width: u32,
    height: u32,
    tiles: &mut TileStore,
    version: u16,
) -> Result<(), String> {
    match op {
        HistoryOp::Patch { layer, regions } => {
            let region_bytes = regions
                .len()
                .checked_mul(MIN_ENCODED_PATCH_REGION_BYTES)
                .and_then(|len| len.checked_add(8))
                .ok_or_else(|| "パッチ情報が大きすぎます".to_owned())?;
            try_reserve_exact(out, region_bytes, "パッチ情報")?;
            put_len(out, *layer, "レイヤー番号")?;
            put_len(out, regions.len(), "パッチ数")?;
            for region in regions {
                put_i32(out, region.rect.x0);
                put_i32(out, region.rect.y0);
                put_i32(out, region.rect.x1);
                put_i32(out, region.rect.y1);
                put_u32(out, tiles.intern(&region.before)?);
                put_u32(out, tiles.intern(&region.after)?);
            }
        }
        HistoryOp::AddLayer {
            index,
            name,
            before_active,
            visible,
            opacity,
            blend,
            alpha_lock,
        } => {
            let encoded_len = 16usize
                .checked_add(name.len())
                .ok_or_else(|| "レイヤー追加情報が大きすぎます".to_owned())?;
            try_reserve_exact(out, encoded_len, "レイヤー追加情報")?;
            put_len(out, *index, "レイヤー番号")?;
            put_len(out, *before_active, "レイヤー番号")?;
            put_string(out, name)?;
            // v10 §47(v2): 追加レイヤーの表示/不透明度。v1 で書く場合
            // (テストの互換検証用のみ)は省略する。
            if version >= 2 {
                put_u8(out, u8::from(*visible));
                put_u8(out, *opacity);
                // v12 §50.4(v3): ブレンド + フラグ(Layer レコードと同じ
                // 2 バイト表現)。v2 以下では書かない。
                if version >= 3 {
                    put_layer_meta(out, *blend, *alpha_lock, version);
                }
            }
        }
        HistoryOp::DuplicateLayer {
            index,
            layer,
            before_active,
        }
        | HistoryOp::RemoveLayer {
            index,
            layer,
            before_active,
        } => {
            try_reserve_exact(out, 8, "レイヤー履歴情報")?;
            put_len(out, *index, "レイヤー番号")?;
            put_len(out, *before_active, "レイヤー番号")?;
            encode_layer(out, layer, width, height, tiles, version)?;
        }
        HistoryOp::MoveLayer { from, to } => {
            try_reserve_exact(out, 8, "レイヤー移動情報")?;
            put_len(out, *from, "レイヤー番号")?;
            put_len(out, *to, "レイヤー番号")?;
        }
        HistoryOp::MergeDown {
            index,
            upper,
            lower_before,
            merged_name,
            merged_visible,
            merged_opacity,
            merged_blend,
            merged_alpha_lock,
        } => {
            try_reserve_exact(out, 8, "レイヤー結合情報")?;
            put_len(out, *index, "レイヤー番号")?;
            // v10 §47(v2): 結合結果レイヤーのメタを upper/lower の前に置く。
            if version >= 2 {
                put_string(out, merged_name)?;
                put_u8(out, u8::from(*merged_visible));
                put_u8(out, *merged_opacity);
                // v12 §50.4(v3): 結合結果のブレンド + フラグ。
                if version >= 3 {
                    put_layer_meta(out, *merged_blend, *merged_alpha_lock, version);
                }
            }
            encode_layer(out, upper, width, height, tiles, version)?;
            encode_layer(out, lower_before, width, height, tiles, version)?;
        }
        HistoryOp::ReplaceAll { before, after } => {
            encode_snapshot(out, before, tiles, version)?;
            encode_snapshot(out, after, tiles, version)?;
        }
    }
    Ok(())
}

fn encode_entry(
    out: &mut Vec<u8>,
    entry: &HistoryEntry,
    sequence: usize,
    width: u32,
    height: u32,
    tiles: &mut TileStore,
    version: u16,
) -> Result<(), String> {
    let revision = u64::try_from(sequence.saturating_add(1))
        .map_err(|_| "履歴番号が大きすぎます".to_owned())?;
    let encoded_len = 40usize
        .checked_add(entry.label.len())
        .ok_or_else(|| "履歴ラベルが大きすぎます".to_owned())?;
    try_reserve_exact(out, encoded_len, "リビジョン情報")?;
    put_u64(out, revision);
    put_u64(out, revision - 1);
    put_u64(out, revision);
    put_u8(
        out,
        u8::from(revision.is_multiple_of(u64::from(CHECKPOINT_INTERVAL))),
    );
    put_u8(out, op_kind(&entry.op));
    put_u16(out, 0);
    put_u32(out, width);
    put_u32(out, height);
    put_string(out, &entry.label)?;
    encode_op(out, &entry.op, width, height, tiles, version)
}

/// v8 レビュー修正③以降、メモリ組み立ては `encode_project`(テスト専用)しか
/// 使わないため、その従属関数ごと `#[cfg(test)]`(出荷バイナリに死んだ
/// コードを残さない)。本体の保存経路は `save` + `write_chunk_to`。
#[cfg(test)]
fn checked_projected_file_len(current: usize, payload: usize) -> Result<usize, String> {
    let projected = u64::try_from(current)
        .ok()
        .and_then(|len| len.checked_add(4 + 8 + 4))
        .and_then(|len| len.checked_add(payload as u64))
        .ok_or_else(|| "プロジェクトファイルが大きすぎます".to_owned())?;
    if projected > MAX_FILE_BYTES {
        return Err("プロジェクトファイルが安全上限を超えています".to_owned());
    }
    usize::try_from(projected).map_err(|_| "プロジェクトファイルが大きすぎます".to_owned())
}

#[cfg(test)]
fn append_chunk(out: &mut Vec<u8>, tag: [u8; 4], payload: &[u8]) -> Result<(), String> {
    if payload.len() > MAX_CHUNK_BYTES {
        return Err("プロジェクトchunkが大きすぎます".to_owned());
    }
    let projected_len = checked_projected_file_len(out.len(), payload.len())?;
    try_reserve_exact(out, projected_len - out.len(), "プロジェクトchunk")?;
    out.extend_from_slice(&tag);
    put_u64(
        out,
        u64::try_from(payload.len()).map_err(|_| "chunk長が大きすぎます".to_owned())?,
    );
    put_u32(out, crc32(payload));
    out.extend_from_slice(payload);
    Ok(())
}

fn budget_layer_heap(budget: &mut ProjectMemoryBudget, layer: &Layer) -> Result<(), String> {
    budget.add_heap_buffer(layer.name.len())?;
    budget.add_heap_buffer(layer.pixels.len())
}

fn budget_snapshot_allocations(
    budget: &mut ProjectMemoryBudget,
    snapshot: &DocSnapshot,
) -> Result<(), String> {
    budget.add_vec::<Layer>(snapshot.layers.len())?;
    for layer in &snapshot.layers {
        budget_layer_heap(budget, layer)?;
    }
    Ok(())
}

/// 保存成功した自作ファイルが、同じ安全上限を使うloaderで必ず受理可能に
/// なるよう、decode時に同時常駐するallocationをwriter側でも同じ分類で
/// 事前計算する。
fn loaded_project_memory_bytes(
    doc: &Document,
    history: &History,
    encoded_len: usize,
    tile_count: usize,
) -> Result<usize, String> {
    let (undo, redo) = history.project_entries();
    let entry_count = undo
        .len()
        .checked_add(redo.len())
        .ok_or_else(|| "履歴数が大きすぎます".to_owned())?;
    let mut budget = ProjectMemoryBudget::with_encoded_len(encoded_len)?;
    budget.add_vec::<&[u8]>(tile_count)?;
    budget.add_vec::<DecodedRevision>(entry_count)?;
    budget.add_vec::<HistoryEntry>(entry_count)?;
    budget.add_vec::<Layer>(doc.layers.len())?;
    budget.add_vec::<&Layer>(doc.layers.len())?;
    for layer in &doc.layers {
        budget_layer_heap(&mut budget, layer)?;
    }
    budget.add_heap_buffer(pixel_len(doc.width, doc.height)?)?;

    for entry in undo.iter().chain(redo) {
        budget.add_heap_buffer(entry.label.len())?;
        match &entry.op {
            HistoryOp::Patch { regions, .. } => {
                budget.add_vec::<PatchRegion>(regions.len())?;
                for region in regions {
                    budget.add_heap_buffer(region.before.len())?;
                    budget.add_heap_buffer(region.after.len())?;
                }
            }
            HistoryOp::AddLayer { name, .. } => budget.add_heap_buffer(name.len())?,
            HistoryOp::DuplicateLayer { layer, .. } | HistoryOp::RemoveLayer { layer, .. } => {
                budget_layer_heap(&mut budget, layer)?;
            }
            HistoryOp::MoveLayer { .. } => {}
            HistoryOp::MergeDown {
                upper,
                lower_before,
                merged_name,
                ..
            } => {
                budget_layer_heap(&mut budget, upper)?;
                budget_layer_heap(&mut budget, lower_before)?;
                // v10 §47: 結合結果メタの名前(v2 はファイルから読み、v1 は
                // 導出 clone を decode 側が手動算入する — どちらも常駐する)。
                budget.add_heap_buffer(merged_name.len())?;
            }
            HistoryOp::ReplaceAll { before, after } => {
                budget_snapshot_allocations(&mut budget, before)?;
                budget_snapshot_allocations(&mut budget, after)?;
            }
        }
    }
    Ok(budget.bytes)
}

/// v8 レビュー修正③(ストリーム書き出し): 4 つの chunk ペイロードと予測
/// 全長。`save` はこれを一時ファイルへ直接流し込み、**最終ファイル全体を
/// もう 1 本の `Vec` に組み立てない**(従来はペイロード群+完成ファイルで
/// ピーク時に約 2 倍のメモリを要した)。`encode_project`(テスト・検証用の
/// メモリ組み立て版)と `save` が同じペイロード生成を共有する。
struct EncodedPayloads {
    meta: Vec<u8>,
    tiles: Vec<u8>,
    document: Vec<u8>,
    revisions: Vec<u8>,
    predicted_len: usize,
}

fn encode_project_payloads(
    doc: &Document,
    history: &History,
    version: u16,
) -> Result<EncodedPayloads, String> {
    let (undo, redo) = history.project_entries();
    let display_step_limit = history.display_step_limit();
    if !(1..=MAX_DISPLAY_STEPS).contains(&display_step_limit) {
        return Err("履歴パネルの表示件数が対応範囲外です".to_owned());
    }
    let entry_count = undo
        .len()
        .checked_add(redo.len())
        .ok_or_else(|| "履歴数が大きすぎます".to_owned())?;
    if entry_count >= MAX_REVISIONS || entry_count > u32::MAX as usize {
        return Err("履歴数が上限を超えました".to_owned());
    }

    let mut tiles = TileStore::default();
    let mut document_payload = Vec::new();
    encode_document(&mut document_payload, doc, &mut tiles, version)?;

    let mut revisions_payload = Vec::new();
    try_reserve_exact(&mut revisions_payload, 8, "履歴ヘッダ")?;
    put_len(&mut revisions_payload, entry_count, "履歴数")?;
    put_len(&mut revisions_payload, undo.len(), "履歴カーソル")?;
    let mut dimensions = (doc.width, doc.height);
    for entry in undo.iter().rev() {
        if let HistoryOp::ReplaceAll { before, .. } = &entry.op {
            dimensions = (before.width, before.height);
        }
    }
    for (sequence, entry) in undo.iter().chain(redo.iter().rev()).enumerate() {
        encode_entry(
            &mut revisions_payload,
            entry,
            sequence,
            dimensions.0,
            dimensions.1,
            &mut tiles,
            version,
        )?;
        if let HistoryOp::ReplaceAll { after, .. } = &entry.op {
            dimensions = (after.width, after.height);
        }
    }

    let tiles_payload_len = tiles.tiles.iter().try_fold(4usize, |total, tile| {
        total
            .checked_add(12)
            .and_then(|value| value.checked_add(tile.len()))
            .ok_or_else(|| "タイルchunkが大きすぎます".to_owned())
    })?;
    if tiles_payload_len > MAX_CHUNK_BYTES {
        return Err("プロジェクトchunkが大きすぎます".to_owned());
    }
    let predicted_encoded_len = (HEADER_SIZE as usize)
        .checked_add(16 * 4)
        .and_then(|len| len.checked_add(24))
        .and_then(|len| len.checked_add(tiles_payload_len))
        .and_then(|len| len.checked_add(document_payload.len()))
        .and_then(|len| len.checked_add(revisions_payload.len()))
        .ok_or_else(|| "プロジェクトファイルが大きすぎます".to_owned())?;
    if predicted_encoded_len as u64 > MAX_FILE_BYTES {
        return Err("プロジェクトファイルが安全上限を超えています".to_owned());
    }
    let _ = loaded_project_memory_bytes(doc, history, predicted_encoded_len, tiles.tiles.len())?;

    let mut tiles_payload = Vec::new();
    try_reserve_exact(&mut tiles_payload, tiles_payload_len, "タイルchunk")?;
    put_len(&mut tiles_payload, tiles.tiles.len(), "タイル数")?;
    for tile in &tiles.tiles {
        put_u64(&mut tiles_payload, content_hash(tile));
        put_len(&mut tiles_payload, tile.len(), "タイル")?;
        tiles_payload.extend_from_slice(tile);
    }

    let mut meta_payload = Vec::new();
    try_reserve_exact(&mut meta_payload, 24, "META chunk")?;
    put_len(
        &mut meta_payload,
        entry_count.saturating_add(1),
        "リビジョン数",
    )?;
    put_len(&mut meta_payload, undo.len(), "現在リビジョン")?;
    put_len(&mut meta_payload, tiles.tiles.len(), "タイル数")?;
    put_u32(&mut meta_payload, CHECKPOINT_INTERVAL);
    put_len(&mut meta_payload, display_step_limit, "キャッシュヒント")?;
    put_u32(&mut meta_payload, 0);

    Ok(EncodedPayloads {
        meta: meta_payload,
        tiles: tiles_payload,
        document: document_payload,
        revisions: revisions_payload,
        predicted_len: predicted_encoded_len,
    })
}

/// テスト・検証用のメモリ組み立て版(`save` は `encode_project_payloads` +
/// ストリーム書き出しを使う。両者が同一のバイト列を生むことは
/// `streamed_save_matches_in_memory_encoding` が担保する)。
#[cfg(test)]
fn encode_project(doc: &Document, history: &History) -> Result<Vec<u8>, String> {
    encode_project_with_version(doc, history, VERSION)
}

/// v10 §47: 旧バージョンのファイルを生成できるテスト専用エンコーダ
/// (後方互換の検証 — v0.7〜v0.9 が書いた v1 ファイルを現行 loader が
/// 読めることを、実際の v1 バイト列で担保する)。
#[cfg(test)]
fn encode_project_with_version(
    doc: &Document,
    history: &History,
    version: u16,
) -> Result<Vec<u8>, String> {
    let payloads = encode_project_payloads(doc, history, version)?;
    let mut out = Vec::new();
    try_reserve_exact(&mut out, payloads.predicted_len, "プロジェクトファイル")?;
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, version);
    put_u8(&mut out, ENDIAN_LITTLE);
    put_u8(&mut out, HEADER_SIZE);
    put_u32(&mut out, 0);
    append_chunk(&mut out, CHUNK_META, &payloads.meta)?;
    append_chunk(&mut out, CHUNK_TILES, &payloads.tiles)?;
    append_chunk(&mut out, CHUNK_DOCUMENT, &payloads.document)?;
    append_chunk(&mut out, CHUNK_REVISIONS, &payloads.revisions)?;
    if out.len() as u64 > MAX_FILE_BYTES {
        return Err("プロジェクトファイルが安全上限を超えています".to_owned());
    }
    if out.len() != payloads.predicted_len {
        return Err("プロジェクトの符号化長が一致しません".to_owned());
    }
    Ok(out)
}

struct ChunkSet<'a> {
    /// v10 §47: ヘッダのバージョン(1 または 2)。REVS chunk 内の一部 op の
    /// 符号化(AddLayer/MergeDown のメタフィールド)がこれで分岐する。
    version: u16,
    meta: &'a [u8],
    tiles: &'a [u8],
    document: &'a [u8],
    revisions: &'a [u8],
}

fn parse_chunks(bytes: &[u8]) -> Result<ChunkSet<'_>, String> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("プロジェクトファイルが大きすぎます".to_owned());
    }
    let mut reader = Reader::new(bytes);
    if reader.take(MAGIC.len())? != MAGIC {
        return Err("Darask Paint プロジェクトではありません".to_owned());
    }
    // v10 §47: v1(v0.7〜v0.9 が書いた形式)と v2(現行)を受け付ける。
    // 書き込みは常に v2(`VERSION`)。
    let version = reader.u16()?;
    if !(1..=VERSION).contains(&version) {
        return Err("未対応の .dpaint バージョンです".to_owned());
    }
    if reader.u8()? != ENDIAN_LITTLE {
        return Err("未対応のエンディアンです".to_owned());
    }
    if reader.u8()? != HEADER_SIZE {
        return Err("プロジェクトヘッダ長が不正です".to_owned());
    }
    if reader.u32()? != 0 {
        return Err("未対応のプロジェクトフラグです".to_owned());
    }

    let mut meta = None;
    let mut tiles = None;
    let mut document = None;
    let mut revisions = None;
    while reader.remaining() != 0 {
        let tag: [u8; 4] = reader
            .take(4)?
            .try_into()
            .map_err(|_| "chunk種別を読めません".to_owned())?;
        let len_u64 = reader.u64()?;
        let len = usize::try_from(len_u64).map_err(|_| "chunk長が大きすぎます".to_owned())?;
        if len > MAX_CHUNK_BYTES || len > reader.remaining().saturating_sub(4) {
            return Err("chunk長が不正です".to_owned());
        }
        let expected_crc = reader.u32()?;
        let payload = reader.take(len)?;
        if crc32(payload) != expected_crc {
            return Err("プロジェクトのCRCが一致しません".to_owned());
        }
        let target = match tag {
            CHUNK_META => Some(&mut meta),
            CHUNK_TILES => Some(&mut tiles),
            CHUNK_DOCUMENT => Some(&mut document),
            CHUNK_REVISIONS => Some(&mut revisions),
            _ => None,
        };
        if let Some(target) = target {
            if target.replace(payload).is_some() {
                return Err("同じ種類のchunkが重複しています".to_owned());
            }
        }
    }
    Ok(ChunkSet {
        version,
        meta: meta.ok_or_else(|| "META chunkがありません".to_owned())?,
        tiles: tiles.ok_or_else(|| "TILS chunkがありません".to_owned())?,
        document: document.ok_or_else(|| "DOCS chunkがありません".to_owned())?,
        revisions: revisions.ok_or_else(|| "REVS chunkがありません".to_owned())?,
    })
}

struct Meta {
    revision_count: usize,
    cursor: usize,
    tile_count: usize,
    display_step_limit: usize,
}

fn decode_meta(bytes: &[u8]) -> Result<Meta, String> {
    let mut reader = Reader::new(bytes);
    let revision_count = reader.u32()? as usize;
    let cursor = reader.u32()? as usize;
    let tile_count = reader.u32()? as usize;
    let checkpoint_interval = reader.u32()?;
    let display_step_limit = reader.u32()? as usize;
    let flags = reader.u32()?;
    reader.finish("META chunk")?;
    if revision_count == 0
        || revision_count > MAX_REVISIONS
        || cursor >= revision_count
        || tile_count > MAX_TILES
        || checkpoint_interval != CHECKPOINT_INTERVAL
        || !(1..=MAX_DISPLAY_STEPS).contains(&display_step_limit)
        || flags != 0
    {
        return Err("META chunkの値が不正です".to_owned());
    }
    Ok(Meta {
        revision_count,
        cursor,
        tile_count,
        display_step_limit,
    })
}

fn decode_tiles<'a>(
    bytes: &'a [u8],
    expected_count: usize,
    budget: &mut ProjectMemoryBudget,
) -> Result<Vec<&'a [u8]>, String> {
    let mut reader = Reader::new(bytes);
    let count = reader.u32()? as usize;
    if count != expected_count || count > MAX_TILES {
        return Err("タイル数がMETAと一致しません".to_owned());
    }
    require_minimum_remaining(&reader, count, MIN_ENCODED_TILE_BYTES, "タイル")?;
    let max_tile_bytes = TILE_SIZE as usize * TILE_SIZE as usize * 4;
    budget.add_vec::<&[u8]>(count)?;
    let mut tiles = Vec::new();
    try_reserve_exact(&mut tiles, count, "タイル参照")?;
    for _ in 0..count {
        let expected_hash = reader.u64()?;
        let len = reader.u32()? as usize;
        if len == 0 || len > max_tile_bytes {
            return Err("タイル長が不正です".to_owned());
        }
        let tile = reader.take(len)?;
        if content_hash(tile) != expected_hash {
            return Err("タイルのcontent hashが一致しません".to_owned());
        }
        tiles.push(tile);
    }
    reader.finish("TILS chunk")?;
    Ok(tiles)
}

fn tile_by_id<'a>(tiles: &[&'a [u8]], id: u32) -> Result<&'a [u8], String> {
    tiles
        .get(id as usize)
        .copied()
        .ok_or_else(|| "存在しないタイル参照です".to_owned())
}

fn decode_layer(
    reader: &mut Reader<'_>,
    tiles: &[&[u8]],
    budget: &mut ProjectMemoryBudget,
    version: u16,
) -> Result<(Layer, u32, u32), String> {
    let width = reader.u32()?;
    let height = reader.u32()?;
    let expected_len = pixel_len(width, height)?;
    let name = reader.string(budget)?;
    let visible = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err("レイヤー表示値が不正です".to_owned()),
    };
    let opacity = reader.u8()?;
    // v12 §50.4: v3 は同じ 2 バイトを blend + flags として読む(v1/v2 は
    // 0 検証のうえ通常/ロック無しで補完)。
    let (blend, alpha_lock) = read_layer_meta(reader, version)?;
    let columns = reader.u32()?;
    let rows = reader.u32()?;
    let count = reader.u32()?;
    let expected_grid = tile_grid(width, height);
    let expected_count = expected_grid
        .0
        .checked_mul(expected_grid.1)
        .ok_or_else(|| "タイル数が大きすぎます".to_owned())?;
    if (columns, rows) != expected_grid || count != expected_count {
        return Err("レイヤーのタイル格子が不正です".to_owned());
    }
    require_minimum_remaining(reader, count as usize, 4, "レイヤータイル参照")?;
    budget.add_heap_buffer(expected_len)?;
    let mut pixels = Vec::new();
    try_reserve_exact(&mut pixels, expected_len, "レイヤー画素")?;
    pixels.resize(expected_len, 0);
    for ty in 0..rows {
        for tx in 0..columns {
            let tile = tile_by_id(tiles, reader.u32()?)?;
            let x = tx * TILE_SIZE;
            let y = ty * TILE_SIZE;
            let tile_width = TILE_SIZE.min(width - x);
            let tile_height = TILE_SIZE.min(height - y);
            let tile_len = pixel_len(tile_width, tile_height)?;
            if tile.len() != tile_len {
                return Err("参照タイルの寸法が一致しません".to_owned());
            }
            let row_bytes = tile_width as usize * 4;
            for row in 0..tile_height {
                let src_start = row as usize * row_bytes;
                let dst_start = ((y + row) as usize * width as usize + x as usize) * 4;
                pixels[dst_start..dst_start + row_bytes]
                    .copy_from_slice(&tile[src_start..src_start + row_bytes]);
            }
        }
    }
    Ok((
        Layer {
            // UID は `.dpaint` に保存しない実行時 ID(v12 §53)。
            uid: next_layer_uid(),
            name,
            visible,
            opacity,
            blend,
            alpha_lock,
            pixels,
        },
        width,
        height,
    ))
}

fn decode_snapshot(
    reader: &mut Reader<'_>,
    tiles: &[&[u8]],
    budget: &mut ProjectMemoryBudget,
    version: u16,
) -> Result<DocSnapshot, String> {
    let width = reader.u32()?;
    let height = reader.u32()?;
    pixel_len(width, height)?;
    let active = reader.u32()? as usize;
    let layer_count = reader.u32()? as usize;
    if layer_count == 0 || layer_count > MAX_LAYERS || active >= layer_count {
        return Err("スナップショットのレイヤー構造が不正です".to_owned());
    }
    require_minimum_remaining(
        reader,
        layer_count,
        MIN_ENCODED_LAYER_BYTES,
        "スナップショットレイヤー",
    )?;
    budget.add_vec::<Layer>(layer_count)?;
    let mut layers = Vec::new();
    try_reserve_exact(&mut layers, layer_count, "スナップショットレイヤー")?;
    for _ in 0..layer_count {
        let (layer, layer_width, layer_height) = decode_layer(reader, tiles, budget, version)?;
        if (layer_width, layer_height) != (width, height) {
            return Err("レイヤー寸法がドキュメントと一致しません".to_owned());
        }
        layers.push(layer);
    }
    Ok(DocSnapshot {
        width,
        height,
        layers,
        active,
    })
}

fn decode_index(reader: &mut Reader<'_>) -> Result<usize, String> {
    Ok(reader.u32()? as usize)
}

/// v10 §47: `version >= 2` のとき 0/1 のフラグバイトを読む(それ以外の値は
/// 不正入力として拒否)。
fn decode_bool(reader: &mut Reader<'_>) -> Result<bool, String> {
    match reader.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("真偽フラグが不正です".to_owned()),
    }
}

fn decode_op(
    reader: &mut Reader<'_>,
    kind: u8,
    version: u16,
    dimensions: (u32, u32),
    tiles: &[&[u8]],
    budget: &mut ProjectMemoryBudget,
) -> Result<HistoryOp, String> {
    match kind {
        1 => {
            let layer = decode_index(reader)?;
            let region_count = reader.u32()? as usize;
            if layer >= MAX_LAYERS || region_count > MAX_TILES {
                return Err("パッチ情報が上限を超えています".to_owned());
            }
            require_minimum_remaining(
                reader,
                region_count,
                MIN_ENCODED_PATCH_REGION_BYTES,
                "パッチ領域",
            )?;
            budget.add_vec::<PatchRegion>(region_count)?;
            let mut regions = Vec::new();
            try_reserve_exact(&mut regions, region_count, "パッチ領域")?;
            // v8 レビュー修正③(SPEC §36 の防御強化): writer(StrokeRecorder)は
            // 常に「256px タイル格子に整列し、ドキュメント境界でだけ切り
            // 詰められた」矩形を重複なく書く。CRC が正しくても、格子に
            // 合わない・重複する領域を持つ外部ファイルは順序依存の
            // before/after を構成できるため拒否する。重複判定はタイル格子の
            // ビットセット(8192² でも 1024 bit)で行う。
            let (grid_columns, grid_rows) = tile_grid(dimensions.0, dimensions.1);
            let grid_cells = (grid_columns as usize) * (grid_rows as usize);
            let mut seen_tiles = vec![0u64; grid_cells.div_ceil(64)];
            for _ in 0..region_count {
                let rect = IRect {
                    x0: reader.i32()?,
                    y0: reader.i32()?,
                    x1: reader.i32()?,
                    y1: reader.i32()?,
                };
                let clamped = rect.clamp_to(dimensions.0, dimensions.1);
                if rect != clamped
                    || rect.is_empty()
                    || rect.width() > TILE_SIZE as i32
                    || rect.height() > TILE_SIZE as i32
                {
                    return Err("パッチ矩形が不正です".to_owned());
                }
                let tile = TILE_SIZE as i32;
                if rect.x0.rem_euclid(tile) != 0
                    || rect.y0.rem_euclid(tile) != 0
                    || rect.x1 != (rect.x0 + tile).min(dimensions.0 as i32)
                    || rect.y1 != (rect.y0 + tile).min(dimensions.1 as i32)
                {
                    return Err("パッチ矩形がタイル格子に一致しません".to_owned());
                }
                let cell =
                    (rect.y0 / tile) as usize * grid_columns as usize + (rect.x0 / tile) as usize;
                let (word, bit) = (cell / 64, cell % 64);
                let Some(slot) = seen_tiles.get_mut(word) else {
                    return Err("パッチ矩形がタイル格子に一致しません".to_owned());
                };
                if *slot & (1u64 << bit) != 0 {
                    return Err("パッチ領域が重複しています".to_owned());
                }
                *slot |= 1u64 << bit;
                let expected_len = (rect.width() as usize)
                    .checked_mul(rect.height() as usize)
                    .and_then(|count| count.checked_mul(4))
                    .ok_or_else(|| "パッチ寸法が大きすぎます".to_owned())?;
                let before = tile_by_id(tiles, reader.u32()?)?;
                let after = tile_by_id(tiles, reader.u32()?)?;
                if before.len() != expected_len || after.len() != expected_len {
                    return Err("パッチのタイル長が一致しません".to_owned());
                }
                budget.add_heap_buffer(expected_len)?;
                budget.add_heap_buffer(expected_len)?;
                regions.push(PatchRegion {
                    rect,
                    before: try_clone_bytes(before, "パッチbefore画素")?,
                    after: try_clone_bytes(after, "パッチafter画素")?,
                });
            }
            Ok(HistoryOp::Patch { layer, regions })
        }
        2 => {
            let index = decode_index(reader)?;
            let before_active = decode_index(reader)?;
            let name = reader.string(budget)?;
            // v10 §47: v2 は追加レイヤーの表示/不透明度を持つ。v1 は生成時の
            // 既定(`Document::add_layer` と同じ)で補う。
            let (visible, opacity) = if version >= 2 {
                (decode_bool(reader)?, reader.u8()?)
            } else {
                (true, 255)
            };
            // v12 §50.4: v3 は追加レイヤーのブレンド/フラグも持つ。
            let (blend, alpha_lock) = if version >= 3 {
                read_layer_meta(reader, version)?
            } else {
                (crate::document::BlendMode::Normal, false)
            };
            Ok(HistoryOp::AddLayer {
                index,
                before_active,
                name,
                visible,
                opacity,
                blend,
                alpha_lock,
            })
        }
        3 | 4 => {
            let index = decode_index(reader)?;
            let before_active = decode_index(reader)?;
            let (layer, width, height) = decode_layer(reader, tiles, budget, version)?;
            if (width, height) != dimensions {
                return Err("履歴レイヤーの寸法が一致しません".to_owned());
            }
            if kind == 3 {
                Ok(HistoryOp::DuplicateLayer {
                    index,
                    layer,
                    before_active,
                })
            } else {
                Ok(HistoryOp::RemoveLayer {
                    index,
                    layer,
                    before_active,
                })
            }
        }
        5 => Ok(HistoryOp::MoveLayer {
            from: decode_index(reader)?,
            to: decode_index(reader)?,
        }),
        6 => {
            let index = decode_index(reader)?;
            // v10 §47: v2 は結合結果レイヤーのメタ(名前/表示/不透明度)を
            // upper/lower の前に持つ。
            let v2_meta = if version >= 2 {
                let merged_name = reader.string(budget)?;
                let merged_visible = decode_bool(reader)?;
                let merged_opacity = reader.u8()?;
                // v12 §50.4: v3 は結合結果のブレンド/フラグも持つ。
                let (merged_blend, merged_alpha_lock) = if version >= 3 {
                    read_layer_meta(reader, version)?
                } else {
                    (crate::document::BlendMode::Normal, false)
                };
                Some((
                    merged_name,
                    merged_visible,
                    merged_opacity,
                    merged_blend,
                    merged_alpha_lock,
                ))
            } else {
                None
            };
            let (upper, upper_width, upper_height) = decode_layer(reader, tiles, budget, version)?;
            let (lower_before, lower_width, lower_height) =
                decode_layer(reader, tiles, budget, version)?;
            if (upper_width, upper_height) != dimensions
                || (lower_width, lower_height) != dimensions
            {
                return Err("結合履歴のレイヤー寸法が一致しません".to_owned());
            }
            let (merged_name, merged_visible, merged_opacity, merged_blend, merged_alpha_lock) =
                match v2_meta {
                    Some(meta) => meta,
                    None => {
                        // v1 互換: 旧 redo 実装(`apply_after` の旧ハードコード)と
                        // 同じ「下レイヤーの名前・表示 ON・不透明度 255」を導出
                        // する。導出した clone はファイルから読んだ文字列ではない
                        // ため、復元メモリ会計へ手動で算入する
                        // (`loaded_project_memory_bytes` 側と対称)。
                        let derived = lower_before.name.clone();
                        budget.add_heap_buffer(derived.len())?;
                        (
                            derived,
                            true,
                            255,
                            crate::document::BlendMode::Normal,
                            false,
                        )
                    }
                };
            Ok(HistoryOp::MergeDown {
                index,
                upper,
                lower_before,
                merged_name,
                merged_visible,
                merged_opacity,
                merged_blend,
                merged_alpha_lock,
            })
        }
        7 => Ok(HistoryOp::ReplaceAll {
            before: decode_snapshot(reader, tiles, budget, version)?,
            after: decode_snapshot(reader, tiles, budget, version)?,
        }),
        _ => Err("未対応の履歴操作です".to_owned()),
    }
}

struct DecodedRevision {
    entry: HistoryEntry,
    dimensions: (u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RevisionState {
    width: u32,
    height: u32,
    layer_count: usize,
}

impl RevisionState {
    fn from_snapshot(snapshot: &DocSnapshot) -> Self {
        Self {
            width: snapshot.width,
            height: snapshot.height,
            layer_count: snapshot.layers.len(),
        }
    }

    fn dimensions(self) -> (u32, u32) {
        (self.width, self.height)
    }
}

fn invalid_revision_state() -> String {
    "履歴操作とドキュメント状態が一致しません".to_owned()
}

/// カーソル位置の DOCS から undo 側を逆向きにたどり、各操作の直前状態を
/// 復元する。外部入力の添字を `History::undo` へ渡す前に、レイヤー数との
/// 整合をここで保証する。
fn state_before_revision(
    after: RevisionState,
    revision: &DecodedRevision,
) -> Result<RevisionState, String> {
    let invalid = invalid_revision_state;
    match &revision.entry.op {
        HistoryOp::Patch { layer, .. } => {
            if revision.dimensions != after.dimensions() || *layer >= after.layer_count {
                return Err(invalid());
            }
            Ok(after)
        }
        HistoryOp::AddLayer {
            index,
            before_active,
            ..
        }
        | HistoryOp::DuplicateLayer {
            index,
            before_active,
            ..
        } => {
            let before_count = after
                .layer_count
                .checked_sub(1)
                .filter(|count| *count != 0)
                .ok_or_else(invalid)?;
            if revision.dimensions != after.dimensions()
                || *index > before_count
                || *before_active >= before_count
            {
                return Err(invalid());
            }
            Ok(RevisionState {
                layer_count: before_count,
                ..after
            })
        }
        HistoryOp::RemoveLayer {
            index,
            before_active,
            ..
        } => {
            let before_count = after
                .layer_count
                .checked_add(1)
                .filter(|count| *count <= MAX_LAYERS)
                .ok_or_else(invalid)?;
            if revision.dimensions != after.dimensions()
                || *index >= before_count
                || *before_active >= before_count
            {
                return Err(invalid());
            }
            Ok(RevisionState {
                layer_count: before_count,
                ..after
            })
        }
        HistoryOp::MoveLayer { from, to } => {
            if revision.dimensions != after.dimensions()
                || *from >= after.layer_count
                || *to >= after.layer_count
            {
                return Err(invalid());
            }
            Ok(after)
        }
        HistoryOp::MergeDown { index, .. } => {
            let before_count = after
                .layer_count
                .checked_add(1)
                .filter(|count| *count <= MAX_LAYERS)
                .ok_or_else(invalid)?;
            if revision.dimensions != after.dimensions() || *index == 0 || *index >= before_count {
                return Err(invalid());
            }
            Ok(RevisionState {
                layer_count: before_count,
                ..after
            })
        }
        HistoryOp::ReplaceAll {
            before,
            after: op_after,
        } => {
            let before = RevisionState::from_snapshot(before);
            let op_after = RevisionState::from_snapshot(op_after);
            if revision.dimensions != before.dimensions() || after != op_after {
                return Err(invalid());
            }
            Ok(before)
        }
    }
}

/// 直前状態からリビジョンを順方向へ意味検証し、直後状態を返す。
fn state_after_revision(
    before: RevisionState,
    revision: &DecodedRevision,
) -> Result<RevisionState, String> {
    let invalid = invalid_revision_state;
    if revision.dimensions != before.dimensions() {
        return Err(invalid());
    }
    match &revision.entry.op {
        HistoryOp::Patch { layer, .. } => {
            if *layer >= before.layer_count {
                return Err(invalid());
            }
            Ok(before)
        }
        HistoryOp::AddLayer {
            index,
            before_active,
            ..
        }
        | HistoryOp::DuplicateLayer {
            index,
            before_active,
            ..
        } => {
            if before.layer_count >= MAX_LAYERS
                || *index > before.layer_count
                || *before_active >= before.layer_count
            {
                return Err(invalid());
            }
            Ok(RevisionState {
                layer_count: before.layer_count + 1,
                ..before
            })
        }
        HistoryOp::RemoveLayer {
            index,
            before_active,
            ..
        } => {
            if before.layer_count <= 1
                || *index >= before.layer_count
                || *before_active >= before.layer_count
            {
                return Err(invalid());
            }
            Ok(RevisionState {
                layer_count: before.layer_count - 1,
                ..before
            })
        }
        HistoryOp::MoveLayer { from, to } => {
            if *from >= before.layer_count || *to >= before.layer_count {
                return Err(invalid());
            }
            Ok(before)
        }
        HistoryOp::MergeDown { index, .. } => {
            if before.layer_count <= 1 || *index == 0 || *index >= before.layer_count {
                return Err(invalid());
            }
            Ok(RevisionState {
                layer_count: before.layer_count - 1,
                ..before
            })
        }
        HistoryOp::ReplaceAll {
            before: op_before,
            after,
        } => {
            if before != RevisionState::from_snapshot(op_before) {
                return Err(invalid());
            }
            Ok(RevisionState::from_snapshot(after))
        }
    }
}

fn validate_revision_states(
    revisions: &[DecodedRevision],
    cursor: usize,
    current: RevisionState,
) -> Result<(), String> {
    let mut root = current;
    for revision in revisions[..cursor].iter().rev() {
        root = state_before_revision(root, revision)?;
    }

    let mut state = root;
    if cursor == 0 && state != current {
        return Err(invalid_revision_state());
    }
    for (index, revision) in revisions.iter().enumerate() {
        state = state_after_revision(state, revision)?;
        if index + 1 == cursor && state != current {
            return Err(invalid_revision_state());
        }
    }
    Ok(())
}

fn decode_revisions(
    bytes: &[u8],
    version: u16,
    meta: &Meta,
    current: RevisionState,
    tiles: &[&[u8]],
    budget: &mut ProjectMemoryBudget,
) -> Result<(Vec<HistoryEntry>, Vec<HistoryEntry>), String> {
    let mut reader = Reader::new(bytes);
    let entry_count = reader.u32()? as usize;
    let cursor = reader.u32()? as usize;
    if entry_count.saturating_add(1) != meta.revision_count
        || cursor != meta.cursor
        || cursor > entry_count
    {
        return Err("履歴カーソルがMETAと一致しません".to_owned());
    }

    require_minimum_remaining(
        &reader,
        entry_count,
        MIN_ENCODED_REVISION_BYTES,
        "リビジョン",
    )?;
    // 検証中のDecodedRevision列と、検証後に所有権を移すundo/redo列が
    // 一時的に同時常駐する分を保存側と同じ予算へ含める。
    budget.add_vec::<DecodedRevision>(entry_count)?;
    budget.add_vec::<HistoryEntry>(entry_count)?;
    let mut revisions = Vec::new();
    try_reserve_exact(&mut revisions, entry_count, "リビジョン")?;
    for sequence in 0..entry_count {
        let revision = reader.u64()?;
        let parent = reader.u64()?;
        let stored_sequence = reader.u64()?;
        let checkpoint = reader.u8()?;
        let kind = reader.u8()?;
        let reserved = reader.u16()?;
        let width = reader.u32()?;
        let height = reader.u32()?;
        pixel_len(width, height)?;
        let expected_revision = sequence as u64 + 1;
        if revision != expected_revision
            || parent != expected_revision - 1
            || stored_sequence != expected_revision
            || checkpoint
                != u8::from(expected_revision.is_multiple_of(u64::from(CHECKPOINT_INTERVAL)))
            || reserved != 0
        {
            return Err("リビジョングラフが不正です".to_owned());
        }
        let label = reader.string(budget)?;
        let op = decode_op(&mut reader, kind, version, (width, height), tiles, budget)?;
        revisions.push(DecodedRevision {
            entry: HistoryEntry { op, label },
            dimensions: (width, height),
        });
    }
    reader.finish("REVS chunk")?;
    validate_revision_states(&revisions, cursor, current)?;
    let mut undo = Vec::new();
    try_reserve_exact(&mut undo, cursor, "undo履歴")?;
    let mut redo = Vec::new();
    try_reserve_exact(&mut redo, entry_count - cursor, "redo履歴")?;
    for (index, revision) in revisions.into_iter().enumerate() {
        if index < cursor {
            undo.push(revision.entry);
        } else {
            redo.push(revision.entry);
        }
    }
    redo.reverse();
    Ok((undo, redo))
}

fn decode_project(bytes: &[u8], path: Option<PathBuf>) -> Result<(Document, History), String> {
    let chunks = parse_chunks(bytes)?;
    let meta = decode_meta(chunks.meta)?;
    // encoded本体と復元先の既知バッファを合算し、同時常駐量が安全上限を
    // 超える入力を画素展開前に拒否する。
    let mut budget = ProjectMemoryBudget::with_encoded_len(bytes.len())?;
    let tiles = decode_tiles(chunks.tiles, meta.tile_count, &mut budget)?;
    let mut document_reader = Reader::new(chunks.document);
    let snapshot = decode_snapshot(&mut document_reader, &tiles, &mut budget, chunks.version)?;
    document_reader.finish("DOCS chunk")?;
    // `Document::try_from_snapshot_owned` が作る合成画像バッファも予算へ含める。
    budget.add_heap_buffer(pixel_len(snapshot.width, snapshot.height)?)?;
    let current = RevisionState::from_snapshot(&snapshot);
    let (undo, redo) = decode_revisions(
        chunks.revisions,
        chunks.version,
        &meta,
        current,
        &tiles,
        &mut budget,
    )?;
    budget.add_vec::<&Layer>(snapshot.layers.len())?;
    let doc = Document::try_from_snapshot_owned(snapshot, path, false)?;
    let history = History::from_project_entries(undo, redo, meta.display_step_limit);
    let expected_budget = loaded_project_memory_bytes(&doc, &history, bytes.len(), tiles.len())?;
    if expected_budget != budget.bytes {
        return Err("プロジェクトの復元メモリ会計が一致しません".to_owned());
    }
    Ok((doc, history))
}

fn unique_sibling(path: &Path, suffix: &str) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    for _ in 0..1000 {
        let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".darask-paint-{}-{nonce}.{suffix}",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("一時ファイル名を確保できませんでした".to_owned())
}

#[cfg(test)]
fn write_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let temp = unique_sibling(path, "tmp")?;
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(temp)
}

fn install_temp(path: &Path, temp: &Path, fail_before_replace: bool) -> Result<(), String> {
    if path.is_dir() {
        let _ = fs::remove_file(temp);
        return Err("保存先がディレクトリです".to_owned());
    }
    if fail_before_replace {
        let _ = fs::remove_file(temp);
        return Err("テスト用の置換失敗".to_owned());
    }
    // tempはpathと同一ディレクトリにあり、close+sync済み。標準ライブラリの
    // renameを1回だけ使うことで、旧実装の「既存pathをbackupへ動かしてから
    // tempを入れる」間に保存先が消えるクラッシュ窓を作らない。Windowsの
    // std実装も既存通常ファイルを置換する。
    if let Err(error) = fs::rename(temp, path) {
        let _ = fs::remove_file(temp);
        return Err(error.to_string());
    }
    Ok(())
}

/// v8 レビュー修正③: 原子的保存のストリーミング版。完成バイト列を
/// メモリへ持たず、一時ファイルへ直接書き込む(`write` クロージャに
/// `BufWriter<File>` を渡す)。成功時は flush→`sync_all`→単一 rename で
/// 設置し、失敗時は一時ファイルを削除して既存ファイルを維持する
/// (`atomic_write` と同じ安全性)。画像書き出し(io.rs — エンコーダを
/// ここへ直接つなぐことで最大 8192² のエンコードバッファ自体を排除)と
/// `.dpaint` の `save` が共有する。
pub(crate) fn atomic_write_with(
    path: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<File>) -> Result<(), String>,
) -> Result<(), String> {
    let temp = unique_sibling(path, "tmp")?;
    let result = (|| -> Result<(), String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| error.to_string())?;
        let mut writer = std::io::BufWriter::new(file);
        write(&mut writer)?;
        let file = writer.into_inner().map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    install_temp(path, &temp, false)
}

/// ストリーミング版の chunk 書き出し(`append_chunk` の `Write` 版)。書いた
/// バイト数を返す(呼び出し側が予測全長との一致を検査する)。
fn write_chunk_to(writer: &mut impl Write, tag: [u8; 4], payload: &[u8]) -> Result<usize, String> {
    if payload.len() > MAX_CHUNK_BYTES {
        return Err("プロジェクトchunkが大きすぎます".to_owned());
    }
    writer.write_all(&tag).map_err(|error| error.to_string())?;
    let len = u64::try_from(payload.len()).map_err(|_| "chunk長が大きすぎます".to_owned())?;
    writer
        .write_all(&len.to_le_bytes())
        .map_err(|error| error.to_string())?;
    writer
        .write_all(&crc32(payload).to_le_bytes())
        .map_err(|error| error.to_string())?;
    writer
        .write_all(payload)
        .map_err(|error| error.to_string())?;
    4usize
        .checked_add(8)
        .and_then(|len| len.checked_add(4))
        .and_then(|len| len.checked_add(payload.len()))
        .ok_or_else(|| "chunk長が大きすぎます".to_owned())
}

/// 現在状態と undo/redo 全件を `.dpaint` へ原子的に保存する。
/// v8 レビュー修正③: 完成ファイルをメモリへ組み立てず、一時ファイルへ
/// chunk 単位でストリーム書き出しする(`EncodedPayloads` のコメント参照)。
/// 書き出した合計長は `encode_project`(メモリ版)と同じ予測全長と一致する
/// ことを検査する(両者のバイト同一性はテストで担保)。
pub fn save(doc: &Document, history: &History, path: &Path) -> Result<(), String> {
    let payloads = encode_project_payloads(doc, history, VERSION)?;
    if payloads.predicted_len as u64 > MAX_FILE_BYTES {
        return Err("プロジェクトファイルが安全上限を超えています".to_owned());
    }
    atomic_write_with(path, |writer| {
        let mut total = MAGIC.len();
        writer.write_all(MAGIC).map_err(|error| error.to_string())?;
        writer
            .write_all(&VERSION.to_le_bytes())
            .map_err(|error| error.to_string())?;
        writer
            .write_all(&[ENDIAN_LITTLE, HEADER_SIZE])
            .map_err(|error| error.to_string())?;
        writer
            .write_all(&0u32.to_le_bytes())
            .map_err(|error| error.to_string())?;
        total += 2 + 2 + 4;
        for (tag, payload) in [
            (CHUNK_META, &payloads.meta),
            (CHUNK_TILES, &payloads.tiles),
            (CHUNK_DOCUMENT, &payloads.document),
            (CHUNK_REVISIONS, &payloads.revisions),
        ] {
            let written = write_chunk_to(writer, tag, payload)?;
            total = total
                .checked_add(written)
                .ok_or_else(|| "プロジェクトファイルが大きすぎます".to_owned())?;
        }
        if total != payloads.predicted_len {
            return Err("プロジェクトの符号化長が一致しません".to_owned());
        }
        Ok(())
    })
}

/// `.dpaint` を検証し、現在状態と全 undo/redo を復元する。
pub fn load(path: &Path) -> Result<(Document, History), String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err("プロジェクトファイルが大きすぎます".to_owned());
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| "プロジェクトファイルが大きすぎます".to_owned())?;
    // metadata検査後にファイルが増えても無制限に読み込まない。巨大な
    // metadata値をそのままreserveせず、段階的に伸ばす。
    let mut bytes = Vec::new();
    try_reserve_exact(
        &mut bytes,
        capacity.min(16 * 1024 * 1024),
        "プロジェクトファイル",
    )?;
    let mut limited = file.take(MAX_FILE_BYTES + 1);
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = limited
            .read(&mut chunk)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        let new_len = bytes
            .len()
            .checked_add(read)
            .ok_or_else(|| "プロジェクトファイルが大きすぎます".to_owned())?;
        if new_len as u64 > MAX_FILE_BYTES {
            return Err("プロジェクトファイルが大きすぎます".to_owned());
        }
        try_reserve_exact(&mut bytes, read, "プロジェクトファイル")?;
        bytes.extend_from_slice(&chunk[..read]);
    }
    decode_project(&bytes, Some(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Background, BlendMode};

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "darask_paint_project_{name}_{}_{}",
            std::process::id(),
            nonce
        ))
    }

    fn assert_snapshot_eq(left: &DocSnapshot, right: &DocSnapshot) {
        assert_eq!((left.width, left.height), (right.width, right.height));
        assert_eq!(left.active, right.active);
        assert_eq!(left.layers.len(), right.layers.len());
        for (left, right) in left.layers.iter().zip(&right.layers) {
            assert_eq!(left.name, right.name);
            assert_eq!(left.visible, right.visible);
            assert_eq!(left.opacity, right.opacity);
            assert_eq!(left.pixels, right.pixels);
        }
    }

    fn assert_single_op_round_trip(
        current: Document,
        before: DocSnapshot,
        op: HistoryOp,
        label: &str,
    ) {
        let after = current.snapshot();
        let mut history = History::new();
        history.push(op, label);
        let encoded = encode_project(&current, &history).expect("encode single op");
        let (mut loaded, mut loaded_history) =
            decode_project(&encoded, None).expect("decode single op");
        assert_snapshot_eq(&loaded.snapshot(), &after);
        assert!(loaded_history.undo(&mut loaded));
        assert_snapshot_eq(&loaded.snapshot(), &before);
        assert!(loaded_history.redo(&mut loaded));
        assert_snapshot_eq(&loaded.snapshot(), &after);
    }

    // -- v10 §47: `.dpaint` v2(レイヤーメタの完全復元)と v1 後方互換 --------

    /// 追加→リネーム/非表示/不透明度変更→undo(op 刷新)→保存→読込→redo で
    /// メタが完全復元されること(v2 の本題)。
    #[test]
    fn v2_round_trip_preserves_meta_edits_across_save_and_redo() {
        let mut doc = Document::new(2, 2, Background::White);
        let mut history = History::new();
        assert!(doc.add_layer("レイヤー 1".to_owned()));
        history.push(
            HistoryOp::AddLayer {
                index: 1,
                name: "レイヤー 1".to_owned(),
                before_active: 0,
                visible: true,
                opacity: 255,
                blend: BlendMode::Normal,
                alpha_lock: false,
            },
            "レイヤーを追加",
        );
        // 追加後のメタ変更(履歴に積まれない、SPEC §13)。
        doc.layers[1].name = "夜空".to_owned();
        doc.layers[1].visible = false;
        doc.layers[1].opacity = 96;
        // undo で op が現在メタへ刷新され、redo 側スタックに載る。
        assert!(history.undo(&mut doc));

        let encoded = encode_project(&doc, &history).expect("encode v2");
        let (mut loaded, mut loaded_history) = decode_project(&encoded, None).expect("decode v2");
        assert!(loaded_history.redo(&mut loaded), "redo the AddLayer");
        let layer = &loaded.layers[1];
        assert_eq!(layer.name, "夜空");
        assert!(!layer.visible);
        assert_eq!(layer.opacity, 96);
    }

    /// 結合→結合結果のメタ変更→undo(刷新)→保存→読込→redo(v2 の
    /// MergeDown 版)。
    #[test]
    fn v2_round_trip_preserves_merged_layer_meta() {
        let mut doc = Document::new(2, 2, Background::White);
        doc.layers
            .push(Layer::filled("上", 2, 2, [10, 20, 30, 255]));
        doc.active = 1;
        let upper = doc.layers[1].clone();
        let lower_before = doc.layers[0].clone();
        let mut history = History::new();
        assert!(doc.merge_active_down());
        history.push(
            HistoryOp::MergeDown {
                index: 1,
                upper,
                lower_before,
                merged_name: doc.layers[0].name.clone(),
                merged_visible: doc.layers[0].visible,
                merged_opacity: doc.layers[0].opacity,
                merged_blend: BlendMode::Normal,
                merged_alpha_lock: false,
            },
            "レイヤーの結合",
        );
        // 結合後のメタ変更。
        doc.layers[0].name = "統合済み".to_owned();
        doc.layers[0].opacity = 128;
        assert!(history.undo(&mut doc));

        let encoded = encode_project(&doc, &history).expect("encode v2");
        let (mut loaded, mut loaded_history) = decode_project(&encoded, None).expect("decode v2");
        assert!(loaded_history.redo(&mut loaded), "redo the MergeDown");
        assert_eq!(loaded.layers[0].name, "統合済み");
        assert_eq!(loaded.layers[0].opacity, 128);
    }

    /// v0.7〜v0.9 が書いた v1 ファイル(旧符号化そのもの)を現行 loader が
    /// 読めること。AddLayer/MergeDown のメタは旧実装と同じ既定へ補われる。
    #[test]
    fn v1_files_still_load_with_default_meta() {
        let mut doc = Document::new(2, 2, Background::White);
        let mut history = History::new();
        assert!(doc.add_layer("レイヤー 1".to_owned()));
        history.push(
            HistoryOp::AddLayer {
                index: 1,
                name: "レイヤー 1".to_owned(),
                before_active: 0,
                visible: true,
                opacity: 255,
                blend: BlendMode::Normal,
                alpha_lock: false,
            },
            "レイヤーを追加",
        );
        let upper = doc.layers[1].clone();
        let lower_before = doc.layers[0].clone();
        assert!(doc.merge_active_down());
        history.push(
            HistoryOp::MergeDown {
                index: 1,
                upper,
                lower_before,
                merged_name: doc.layers[0].name.clone(),
                merged_visible: true,
                merged_opacity: 255,
                merged_blend: BlendMode::Normal,
                merged_alpha_lock: false,
            },
            "レイヤーの結合",
        );

        let encoded_v1 =
            encode_project_with_version(&doc, &history, 1).expect("encode as legacy v1");
        // v1 ヘッダであること(バイトレベルの確認)。
        assert_eq!(&encoded_v1[8..10], &1u16.to_le_bytes());

        let (mut loaded, mut loaded_history) =
            decode_project(&encoded_v1, None).expect("current loader must accept v1");
        // undo 2 回 → redo 2 回が旧来どおり往復する。
        assert!(loaded_history.undo(&mut loaded));
        assert!(loaded_history.undo(&mut loaded));
        assert_eq!(loaded.layers.len(), 1);
        assert!(loaded_history.redo(&mut loaded));
        assert!(loaded_history.redo(&mut loaded));
        assert_eq!(loaded.layers.len(), 1, "結合まで再適用");
        assert_eq!(loaded.layers[0].name, "背景", "v1 既定=下レイヤー名");
    }

    // -- v12 §50.4: `.dpaint` v3(blend + flags)-------------------------

    /// 書き込みは常に v3(ヘッダのバイトを直接確認する)。
    #[test]
    fn saving_writes_version_three_headers() {
        let doc = Document::new(2, 2, Background::White);
        let history = History::new();
        let encoded = encode_project(&doc, &history).expect("encode");
        assert_eq!(&encoded[8..10], &3u16.to_le_bytes());
    }

    /// v3 はレイヤーのブレンド・アルファロックを往復する(現在の文書側)。
    #[test]
    fn v3_round_trips_layer_blend_and_alpha_lock() {
        let mut doc = Document::new(2, 2, Background::White);
        assert!(doc.add_layer("効果".to_owned()));
        doc.layers[0].blend = BlendMode::Multiply;
        doc.layers[0].alpha_lock = true;
        doc.layers[1].blend = BlendMode::Add;
        doc.layers[1].alpha_lock = false;
        let history = History::new();

        let encoded = encode_project(&doc, &history).expect("encode v3");
        let (loaded, _) = decode_project(&encoded, None).expect("decode v3");
        assert_eq!(loaded.layers[0].blend, BlendMode::Multiply);
        assert!(loaded.layers[0].alpha_lock);
        assert_eq!(loaded.layers[1].blend, BlendMode::Add);
        assert!(!loaded.layers[1].alpha_lock);
    }

    /// v3 は AddLayer / MergeDown の redo 用メタ(§50.4)も往復する。
    #[test]
    fn v3_round_trip_preserves_blend_and_alpha_lock_edits_across_save_and_redo() {
        let mut doc = Document::new(2, 2, Background::White);
        let mut history = History::new();
        assert!(doc.add_layer("レイヤー 1".to_owned()));
        history.push(
            HistoryOp::AddLayer {
                index: 1,
                name: "レイヤー 1".to_owned(),
                before_active: 0,
                visible: true,
                opacity: 255,
                blend: BlendMode::Normal,
                alpha_lock: false,
            },
            "レイヤーを追加",
        );
        // 追加後のメタ変更(履歴に積まれない、SPEC §13/§50)。
        doc.layers[1].blend = BlendMode::Overlay;
        doc.layers[1].alpha_lock = true;
        assert!(history.undo(&mut doc));

        let encoded = encode_project(&doc, &history).expect("encode v3");
        let (mut loaded, mut loaded_history) = decode_project(&encoded, None).expect("decode v3");
        assert!(loaded_history.redo(&mut loaded), "redo the AddLayer");
        assert_eq!(loaded.layers[1].blend, BlendMode::Overlay);
        assert!(loaded.layers[1].alpha_lock);
    }

    #[test]
    fn v3_round_trip_preserves_merged_layer_blend_and_alpha_lock() {
        let mut doc = Document::new(2, 2, Background::White);
        let mut history = History::new();
        assert!(doc.add_layer("上".to_owned()));
        let upper = doc.layers[1].clone();
        let lower_before = doc.layers[0].clone();
        assert!(doc.merge_active_down());
        history.push(
            HistoryOp::MergeDown {
                index: 1,
                upper,
                lower_before,
                merged_name: doc.layers[0].name.clone(),
                merged_visible: true,
                merged_opacity: 255,
                merged_blend: BlendMode::Normal,
                merged_alpha_lock: false,
            },
            "レイヤーの結合",
        );
        doc.layers[0].blend = BlendMode::Screen;
        doc.layers[0].alpha_lock = true;
        assert!(history.undo(&mut doc));

        let encoded = encode_project(&doc, &history).expect("encode v3");
        let (mut loaded, mut loaded_history) = decode_project(&encoded, None).expect("decode v3");
        assert!(loaded_history.redo(&mut loaded), "redo the MergeDown");
        assert_eq!(loaded.layers[0].blend, BlendMode::Screen);
        assert!(loaded.layers[0].alpha_lock);
    }

    /// v1/v2 の実バイトは今も読め、ブレンドは「通常」・ロック無しへ補完される
    /// (旧 reserved u16 = 0 の検証は version 分岐に移した —
    /// ARCHITECTURE.md §22.8-8)。
    #[test]
    fn v1_and_v2_files_load_as_normal_blend_without_alpha_lock() {
        let mut doc = Document::new(2, 2, Background::White);
        assert!(doc.add_layer("上".to_owned()));
        // 現在の文書には v3 でしか表現できないメタが載っているが、
        // v1/v2 で書けば単に落ちる(旧形式の意味論どおり)。
        doc.layers[1].blend = BlendMode::Multiply;
        doc.layers[1].alpha_lock = true;
        let history = History::new();

        for version in [1u16, 2] {
            let encoded = encode_project_with_version(&doc, &history, version)
                .unwrap_or_else(|e| panic!("encode as legacy v{version}: {e}"));
            assert_eq!(&encoded[8..10], &version.to_le_bytes());
            let (loaded, _) = decode_project(&encoded, None)
                .unwrap_or_else(|e| panic!("current loader must accept v{version}: {e}"));
            assert_eq!(loaded.layers[1].blend, BlendMode::Normal);
            assert!(!loaded.layers[1].alpha_lock);
        }
    }

    /// v3 の `blend` は 0..=4 のみ(範囲外は拒否 — SPEC §50.4)。
    #[test]
    fn decoding_rejects_an_out_of_range_blend_value_in_v3() {
        let doc = Document::new(2, 2, Background::White);
        let history = History::new();
        let encoded = encode_project(&doc, &history).expect("encode");
        let chunks = parse_chunks(&encoded).expect("parse");
        let mut document = chunks.document.to_vec();
        let meta_at = layer_meta_offset_in_document(&document);
        assert_eq!(document[meta_at], 0, "blend=通常 の位置を特定できた");
        document[meta_at] = 5;
        let rebuilt = rebuild_with_document(&encoded, &document);
        let Err(error) = decode_project(&rebuilt, None) else {
            panic!("blend=5 は拒否されなければならない");
        };
        assert!(error.contains("ブレンド"), "unexpected error: {error}");
    }

    /// v3 の `flags` は bit0(alpha_lock)以外が 0 であること(SPEC §50.4:
    /// 「v3 内での前方拡張はしない」)。
    #[test]
    fn decoding_rejects_unknown_layer_flag_bits_in_v3() {
        let doc = Document::new(2, 2, Background::White);
        let history = History::new();
        let encoded = encode_project(&doc, &history).expect("encode");
        let chunks = parse_chunks(&encoded).expect("parse");
        let mut document = chunks.document.to_vec();
        let flags_at = layer_meta_offset_in_document(&document) + 1;
        assert_eq!(document[flags_at], 0);
        document[flags_at] = 0b10;
        let rebuilt = rebuild_with_document(&encoded, &document);
        let Err(error) = decode_project(&rebuilt, None) else {
            panic!("未知のフラグビットは拒否されなければならない");
        };
        assert!(error.contains("フラグ"), "unexpected error: {error}");

        // bit0 だけなら受理され、アルファロックとして復元される。
        let mut ok = chunks.document.to_vec();
        ok[flags_at] = 0b1;
        let rebuilt = rebuild_with_document(&encoded, &ok);
        let (loaded, _) = decode_project(&rebuilt, None).expect("bit0 は正当なフラグ");
        assert!(loaded.layers[0].alpha_lock);
    }

    /// v1/v2 の `reserved u16` は今も 0 必須(version 分岐が効いていること)。
    #[test]
    fn decoding_rejects_a_non_zero_reserved_field_in_v1() {
        let doc = Document::new(2, 2, Background::White);
        let history = History::new();
        let encoded = encode_project_with_version(&doc, &history, 1).expect("encode v1");
        let chunks = parse_chunks(&encoded).expect("parse");
        let mut document = chunks.document.to_vec();
        let reserved_at = layer_meta_offset_in_document(&document);
        document[reserved_at] = 1;
        let rebuilt = rebuild_with_document(&encoded, &document);
        let Err(error) = decode_project(&rebuilt, None) else {
            panic!("v1 の reserved != 0 は拒否されなければならない");
        };
        assert!(error.contains("予約"), "unexpected error: {error}");
    }

    /// DOCS chunk 内、最初のレイヤーレコードの blend/flags(旧 reserved u16)の
    /// 位置。レイアウト: width/height/active/layer_count(16B)+ レイヤー
    /// (width/height(8B)+ 名前長(4B)+ 名前 + visible(1B)+ opacity(1B))。
    fn layer_meta_offset_in_document(document: &[u8]) -> usize {
        let name_len_at = 16 + 8;
        let name_len = u32::from_le_bytes([
            document[name_len_at],
            document[name_len_at + 1],
            document[name_len_at + 2],
            document[name_len_at + 3],
        ]) as usize;
        name_len_at + 4 + name_len + 2
    }

    /// DOCS chunk を差し替えて CRC を引き直す(`rebuild_with_revisions` の
    /// DOCS 版)。
    fn rebuild_with_document(encoded: &[u8], document: &[u8]) -> Vec<u8> {
        let chunks = parse_chunks(encoded).expect("parse");
        let mut out = encoded[..16].to_vec();
        for (tag, payload) in [
            (CHUNK_META, chunks.meta),
            (CHUNK_TILES, chunks.tiles),
            (CHUNK_DOCUMENT, document),
            (CHUNK_REVISIONS, chunks.revisions),
        ] {
            append_chunk(&mut out, tag, payload).expect("append");
        }
        out
    }

    #[test]
    fn decoding_rejects_an_invalid_bool_flag_in_v2() {
        let mut doc = Document::new(2, 2, Background::White);
        let mut history = History::new();
        assert!(doc.add_layer("追加".to_owned()));
        history.push(
            HistoryOp::AddLayer {
                index: 1,
                name: "追加".to_owned(),
                before_active: 0,
                visible: true,
                opacity: 255,
                blend: BlendMode::Normal,
                alpha_lock: false,
            },
            "追加",
        );
        let encoded = encode_project(&doc, &history).expect("encode");
        // REVS payload 内の visible フラグ(name の直後)を 2 に書き換え、
        // chunk を再構成する(CRC も引き直す)。
        let chunks = parse_chunks(&encoded).expect("parse");
        let mut revisions = chunks.revisions.to_vec();
        let needle = "追加".as_bytes();
        // 2 回目の出現が op 内の name(1 回目はラベル)。その直後が visible。
        let first = revisions
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("label occurrence");
        let second_rel = revisions[first + needle.len()..]
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("op name occurrence");
        let flag_at = first + needle.len() + second_rel + needle.len();
        assert_eq!(revisions[flag_at], 1, "visible=true の位置を特定できた");
        revisions[flag_at] = 2;
        let rebuilt = rebuild_with_revisions(&encoded, &revisions);
        let Err(error) = decode_project(&rebuilt, None) else {
            panic!("visible=2 は拒否されなければならない");
        };
        assert!(error.contains("真偽"), "unexpected error: {error}");
    }

    // -- v8 レビュー修正③: ストリーム書き出し・Patch 領域検証 ----------------

    #[test]
    fn streamed_save_matches_in_memory_encoding() {
        let mut doc = Document::new(300, 300, Background::White);
        let mut history = History::new();
        history.begin_stroke(doc.active);
        history.ensure_tiles_saved(
            &doc,
            IRect {
                x0: 10,
                y0: 10,
                x1: 280,
                y1: 280,
            },
        );
        doc.set_pixel(20, 20, [1, 2, 3, 255]);
        doc.set_pixel(270, 270, [9, 8, 7, 255]);
        history.commit_stroke(&mut doc, "ブラシ");

        let dir = temp_dir("streamed_save");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("streamed.dpaint");
        save(&doc, &history, &path).expect("streamed save");
        let streamed = std::fs::read(&path).expect("read streamed file");
        let in_memory = encode_project(&doc, &history).expect("in-memory encode");
        assert_eq!(streamed, in_memory, "ストリーム版とメモリ版は同一バイト列");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decoding_rejects_a_patch_rect_off_the_tile_grid() {
        // writer(StrokeRecorder)はタイル格子に整列した矩形しか書かない。
        // CRC が正しくても格子に合わない外部ファイルは拒否する(SPEC §36)。
        let doc = Document::new(300, 300, Background::White);
        let mut history = History::new();
        history.push(
            HistoryOp::Patch {
                layer: 0,
                regions: vec![PatchRegion {
                    rect: IRect {
                        x0: 1,
                        y0: 0,
                        x1: 2,
                        y1: 1,
                    },
                    before: vec![0; 4],
                    after: vec![9; 4],
                }],
            },
            "非整列パッチ",
        );
        let encoded = encode_project(&doc, &history).expect("encode");
        let Err(error) = decode_project(&encoded, None) else {
            panic!("a misaligned patch rect must be rejected");
        };
        assert!(error.contains("タイル格子"), "unexpected error: {error}");
    }

    #[test]
    fn decoding_rejects_duplicate_patch_regions() {
        // 同一タイルを 2 回持つ Patch は before/after の適用順で結果が変わる
        // 曖昧な入力(writer は生成しない)。
        let doc = Document::new(300, 300, Background::White);
        let region = || PatchRegion {
            rect: IRect {
                x0: 0,
                y0: 0,
                x1: 256,
                y1: 256,
            },
            before: vec![0; 256 * 256 * 4],
            after: vec![9; 256 * 256 * 4],
        };
        let mut history = History::new();
        history.push(
            HistoryOp::Patch {
                layer: 0,
                regions: vec![region(), region()],
            },
            "重複パッチ",
        );
        let encoded = encode_project(&doc, &history).expect("encode");
        let Err(error) = decode_project(&encoded, None) else {
            panic!("duplicate patch regions must be rejected");
        };
        assert!(error.contains("重複"), "unexpected error: {error}");
    }

    fn rebuild_with_revisions(encoded: &[u8], revisions: &[u8]) -> Vec<u8> {
        let chunks = parse_chunks(encoded).expect("parse source project");
        let mut rebuilt = encoded[..HEADER_SIZE as usize].to_vec();
        append_chunk(&mut rebuilt, CHUNK_META, chunks.meta).expect("META");
        append_chunk(&mut rebuilt, CHUNK_TILES, chunks.tiles).expect("TILS");
        append_chunk(&mut rebuilt, CHUNK_DOCUMENT, chunks.document).expect("DOCS");
        append_chunk(&mut rebuilt, CHUNK_REVISIONS, revisions).expect("REVS");
        rebuilt
    }

    #[test]
    fn v1_round_trip_restores_layers_undo_redo_and_middle_cursor() {
        let mut doc = Document::new(4, 3, Background::Transparent);
        doc.layers.push(Layer {
            uid: next_layer_uid(),
            name: "上レイヤー".to_owned(),
            visible: false,
            opacity: 123,
            blend: BlendMode::Normal,
            alpha_lock: false,
            pixels: vec![0; 4 * 3 * 4],
        });
        doc.active = 1;
        let baseline = doc.snapshot();
        let mut history = History::new();

        let rect_a = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        history.begin_stroke(doc.active);
        history.ensure_tiles_saved(&doc, rect_a);
        doc.set_pixel(0, 0, [10, 20, 30, 40]);
        history.commit_stroke(&mut doc, "ブラシ1");
        let after_first = doc.snapshot();

        let rect_b = IRect {
            x0: 1,
            y0: 0,
            x1: 2,
            y1: 1,
        };
        history.begin_stroke(doc.active);
        history.ensure_tiles_saved(&doc, rect_b);
        doc.set_pixel(1, 0, [50, 60, 70, 80]);
        history.commit_stroke(&mut doc, "ブラシ2");
        let after_second = doc.snapshot();
        assert!(history.undo(&mut doc));
        assert_snapshot_eq(&doc.snapshot(), &after_first);

        let encoded = encode_project(&doc, &history).expect("encode");
        let (mut loaded, mut loaded_history) =
            decode_project(&encoded, None).expect("decode should succeed");
        assert_snapshot_eq(&loaded.snapshot(), &after_first);
        assert_eq!(loaded.layers[1].name, "上レイヤー");
        assert!(!loaded.layers[1].visible);
        assert_eq!(loaded.layers[1].opacity, 123);
        assert_eq!(loaded_history.undo_len(), 1);
        assert!(loaded_history.can_undo());
        assert!(loaded_history.can_redo());

        assert!(loaded_history.undo(&mut loaded));
        assert_snapshot_eq(&loaded.snapshot(), &baseline);
        assert!(loaded_history.redo(&mut loaded));
        assert_snapshot_eq(&loaded.snapshot(), &after_first);
        assert!(loaded_history.redo(&mut loaded));
        assert_snapshot_eq(&loaded.snapshot(), &after_second);
    }

    #[test]
    fn replace_all_dimension_changes_round_trip_with_middle_cursor() {
        let state0 = Document::new(2, 2, Background::White).snapshot();
        let mut state1_doc = Document::new(3, 1, Background::Transparent);
        state1_doc.layers[0].pixels[0..4].copy_from_slice(&[1, 2, 3, 255]);
        let state1 = state1_doc.snapshot();
        let mut state2_doc = Document::new(1, 4, Background::Transparent);
        state2_doc.layers[0].pixels[12..16].copy_from_slice(&[4, 5, 6, 255]);
        let state2 = state2_doc.snapshot();

        let mut history = History::new();
        history.push(
            HistoryOp::ReplaceAll {
                before: state0.clone(),
                after: state1.clone(),
            },
            "サイズ変更1",
        );
        history.push(
            HistoryOp::ReplaceAll {
                before: state1.clone(),
                after: state2.clone(),
            },
            "サイズ変更2",
        );
        let mut current =
            Document::try_from_snapshot_owned(state2.clone(), None, true).expect("document");
        assert!(history.undo(&mut current));
        assert_snapshot_eq(&current.snapshot(), &state1);

        let encoded = encode_project(&current, &history).expect("encode middle cursor");
        let (mut loaded, mut loaded_history) =
            decode_project(&encoded, None).expect("decode middle cursor");
        assert_snapshot_eq(&loaded.snapshot(), &state1);
        assert!(loaded_history.undo(&mut loaded));
        assert_snapshot_eq(&loaded.snapshot(), &state0);
        assert!(loaded_history.redo(&mut loaded));
        assert_snapshot_eq(&loaded.snapshot(), &state1);
        assert!(loaded_history.redo(&mut loaded));
        assert_snapshot_eq(&loaded.snapshot(), &state2);
    }

    #[test]
    fn identical_layer_tiles_are_deduplicated() {
        let mut doc = Document::new(300, 300, Background::Transparent);
        doc.layers.push(doc.layers[0].clone());
        let encoded = encode_project(&doc, &History::new()).expect("encode");
        let chunks = parse_chunks(&encoded).expect("chunks");
        let meta = decode_meta(chunks.meta).expect("meta");
        // 全面・端・隅の3つの異なるbyte列。縦端と横端は同じ長さの全0で
        // content-identicalなため共有され、2枚目も全タイルを再利用する。
        assert_eq!(meta.tile_count, 3);
    }

    #[test]
    fn every_structural_history_op_round_trips() {
        // AddLayer
        let mut doc = Document::new(2, 2, Background::White);
        let before = doc.snapshot();
        doc.layers.push(Layer::filled("追加", 2, 2, [0, 0, 0, 0]));
        doc.active = 1;
        assert_single_op_round_trip(
            doc,
            before,
            HistoryOp::AddLayer {
                index: 1,
                name: "追加".to_owned(),
                before_active: 0,
                visible: true,
                opacity: 255,
                blend: BlendMode::Normal,
                alpha_lock: false,
            },
            "追加",
        );

        // DuplicateLayer
        let mut doc = Document::new(2, 2, Background::Transparent);
        doc.layers[0].pixels[0..4].copy_from_slice(&[1, 2, 3, 255]);
        let before = doc.snapshot();
        let duplicate = doc.layers[0].clone();
        doc.layers.push(duplicate.clone());
        doc.active = 1;
        assert_single_op_round_trip(
            doc,
            before,
            HistoryOp::DuplicateLayer {
                index: 1,
                layer: duplicate,
                before_active: 0,
            },
            "複製",
        );

        // RemoveLayer
        let mut doc = Document::new(2, 2, Background::White);
        doc.layers
            .push(Layer::filled("削除対象", 2, 2, [9, 8, 7, 255]));
        doc.active = 1;
        let before = doc.snapshot();
        let removed = doc.layers.remove(1);
        doc.active = 0;
        assert_single_op_round_trip(
            doc,
            before,
            HistoryOp::RemoveLayer {
                index: 1,
                layer: removed,
                before_active: 1,
            },
            "削除",
        );

        // MoveLayer
        let mut doc = Document::new(2, 2, Background::White);
        doc.layers.push(Layer::filled("上", 2, 2, [1, 2, 3, 255]));
        doc.active = 0;
        let before = doc.snapshot();
        doc.layers.swap(0, 1);
        doc.active = 1;
        assert_single_op_round_trip(doc, before, HistoryOp::MoveLayer { from: 0, to: 1 }, "移動");

        // MergeDown
        let mut doc = Document::new(2, 2, Background::White);
        doc.layers
            .push(Layer::filled("上", 2, 2, [10, 20, 30, 128]));
        doc.active = 1;
        let before = doc.snapshot();
        let lower_before = doc.layers[0].clone();
        let upper = doc.layers[1].clone();
        let merged = crate::document::composite_two(&lower_before, &upper, 2, 2);
        let merged_name = lower_before.name.clone();
        doc.layers[0] = Layer {
            uid: next_layer_uid(),
            name: merged_name.clone(),
            visible: true,
            opacity: 255,
            blend: BlendMode::Normal,
            alpha_lock: false,
            pixels: merged,
        };
        doc.layers.remove(1);
        doc.active = 0;
        assert_single_op_round_trip(
            doc,
            before,
            HistoryOp::MergeDown {
                index: 1,
                upper,
                lower_before,
                merged_name,
                merged_visible: true,
                merged_opacity: 255,
                merged_blend: BlendMode::Normal,
                merged_alpha_lock: false,
            },
            "結合",
        );

        // ReplaceAll（寸法変更を含む）
        let before_doc = Document::new(2, 2, Background::White);
        let before = before_doc.snapshot();
        let mut doc = Document::new(3, 1, Background::Transparent);
        doc.layers[0].pixels[0..4].copy_from_slice(&[4, 5, 6, 255]);
        let after = doc.snapshot();
        assert_single_op_round_trip(
            doc,
            before.clone(),
            HistoryOp::ReplaceAll { before, after },
            "サイズ変更",
        );
    }

    #[test]
    fn crc_valid_revision_with_invalid_layer_index_is_rejected() {
        let mut doc = Document::new(2, 2, Background::Transparent);
        doc.layers.push(Layer::filled("追加", 2, 2, [0, 0, 0, 0]));
        doc.active = 1;
        let mut history = History::new();
        history.push(
            HistoryOp::AddLayer {
                index: 1,
                name: "追加".to_owned(),
                before_active: 0,
                visible: true,
                opacity: 255,
                blend: BlendMode::Normal,
                alpha_lock: false,
            },
            "追加",
        );
        let encoded = encode_project(&doc, &history).expect("encode");
        let chunks = parse_chunks(&encoded).expect("parse");
        let mut revisions = chunks.revisions.to_vec();
        let index_offset = {
            let mut reader = Reader::new(&revisions);
            let mut budget = ProjectMemoryBudget::with_encoded_len(0).expect("budget");
            assert_eq!(reader.u32().expect("count"), 1);
            assert_eq!(reader.u32().expect("cursor"), 1);
            let _ = reader.u64().expect("revision");
            let _ = reader.u64().expect("parent");
            let _ = reader.u64().expect("sequence");
            let _ = reader.u8().expect("checkpoint");
            assert_eq!(reader.u8().expect("kind"), 2);
            let _ = reader.u16().expect("reserved");
            let _ = reader.u32().expect("width");
            let _ = reader.u32().expect("height");
            let _ = reader.string(&mut budget).expect("label");
            reader.pos
        };
        revisions[index_offset..index_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let malicious = rebuild_with_revisions(&encoded, &revisions);
        let error = match decode_project(&malicious, None) {
            Ok(_) => panic!("invalid index must fail"),
            Err(error) => error,
        };
        assert!(error.contains("履歴操作とドキュメント状態"));
    }

    #[test]
    fn final_project_size_limit_is_checked_without_allocating_the_limit() {
        let near_limit = usize::try_from(MAX_FILE_BYTES).expect("limit fits usize");
        assert!(checked_projected_file_len(near_limit - 16, 0).is_ok());
        assert!(checked_projected_file_len(near_limit - 15, 0).is_err());
        assert!(checked_projected_file_len(HEADER_SIZE as usize, usize::MAX).is_err());
    }

    #[test]
    fn shared_memory_budget_has_a_checked_boundary() {
        let encoded_at_limit = MAX_PROJECT_MEMORY_BYTES - PROJECT_MEMORY_SAFETY_BYTES;
        let mut full = ProjectMemoryBudget::with_encoded_len(encoded_at_limit).expect("at limit");
        assert!(full.add_heap_buffer(1).is_err());

        let mut fits =
            ProjectMemoryBudget::with_encoded_len(encoded_at_limit - 16).expect("below limit");
        fits.add_heap_buffer(1).expect("aligned buffer fits");
        assert_eq!(fits.bytes, MAX_PROJECT_MEMORY_BYTES);
    }

    #[test]
    fn truncated_huge_counts_are_rejected_before_reserving() {
        let mut tile_payload = Vec::new();
        put_len(&mut tile_payload, MAX_TILES, "tiles").expect("tile count");
        let mut budget = ProjectMemoryBudget::with_encoded_len(tile_payload.len()).expect("budget");
        assert!(decode_tiles(&tile_payload, MAX_TILES, &mut budget).is_err());

        let mut revision_payload = Vec::new();
        put_len(&mut revision_payload, MAX_REVISIONS - 1, "revisions").expect("revision count");
        put_u32(&mut revision_payload, 0);
        let meta = Meta {
            revision_count: MAX_REVISIONS,
            cursor: 0,
            tile_count: 0,
            display_step_limit: 50,
        };
        let current = RevisionState {
            width: 1,
            height: 1,
            layer_count: 1,
        };
        let mut budget =
            ProjectMemoryBudget::with_encoded_len(revision_payload.len()).expect("budget");
        assert!(
            decode_revisions(&revision_payload, VERSION, &meta, current, &[], &mut budget).is_err()
        );

        let mut patch_payload = Vec::new();
        put_u32(&mut patch_payload, 0);
        put_len(&mut patch_payload, MAX_TILES, "regions").expect("region count");
        let mut reader = Reader::new(&patch_payload);
        let mut budget =
            ProjectMemoryBudget::with_encoded_len(patch_payload.len()).expect("budget");
        assert!(decode_op(&mut reader, 1, VERSION, (1, 1), &[], &mut budget).is_err());
    }

    #[test]
    fn crc_corruption_is_rejected() {
        let doc = Document::new(2, 2, Background::Transparent);
        let mut encoded = encode_project(&doc, &History::new()).expect("encode");
        let last = encoded.len() - 1;
        encoded[last] ^= 0x80;
        assert!(decode_project(&encoded, None).is_err());
    }

    #[test]
    fn truncation_and_huge_chunk_length_are_rejected() {
        let doc = Document::new(2, 2, Background::Transparent);
        let encoded = encode_project(&doc, &History::new()).expect("encode");
        assert!(decode_project(&encoded[..encoded.len() - 1], None).is_err());

        let mut huge = Vec::new();
        huge.extend_from_slice(MAGIC);
        put_u16(&mut huge, VERSION);
        put_u8(&mut huge, ENDIAN_LITTLE);
        put_u8(&mut huge, HEADER_SIZE);
        put_u32(&mut huge, 0);
        huge.extend_from_slice(&CHUNK_META);
        put_u64(&mut huge, u64::MAX);
        put_u32(&mut huge, 0);
        assert!(decode_project(&huge, None).is_err());
    }

    #[test]
    fn unknown_crc_checked_chunk_is_skipped() {
        let doc = Document::new(2, 2, Background::Transparent);
        let encoded = encode_project(&doc, &History::new()).expect("encode");
        let mut with_unknown = encoded[..HEADER_SIZE as usize].to_vec();
        append_chunk(&mut with_unknown, *b"FUTR", b"future data").expect("chunk");
        with_unknown.extend_from_slice(&encoded[HEADER_SIZE as usize..]);
        assert!(decode_project(&with_unknown, None).is_ok());
    }

    #[test]
    fn failed_atomic_install_preserves_original_and_cleans_temp() {
        let dir = temp_dir("atomic");
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("work.dpaint");
        fs::write(&path, b"original").expect("write original");
        let temp = write_temp(&path, b"replacement").expect("write temp");
        assert!(install_temp(&path, &temp, true).is_err());
        assert_eq!(fs::read(&path).expect("read original"), b"original");
        assert!(!temp.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn public_save_atomically_overwrites_and_loads_clean_document() {
        let dir = temp_dir("overwrite");
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("日本語 プロジェクト.dpaint");

        let first = Document::new(2, 2, Background::White);
        save(&first, &History::new(), &path).expect("first save");

        let mut replacement = Document::new(2, 2, Background::Transparent);
        replacement.layers[0].pixels[0..4].copy_from_slice(&[1, 2, 3, 4]);
        replacement.modified = true;
        save(&replacement, &History::new(), &path).expect("overwrite");

        let (loaded, loaded_history) = load(&path).expect("load replacement");
        assert_eq!(&loaded.layers[0].pixels[0..4], &[1, 2, 3, 4]);
        assert_eq!(loaded.path.as_deref(), Some(path.as_path()));
        assert!(!loaded.modified);
        assert!(!loaded_history.can_undo());
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .expect("read temp dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.path() != path)
            .collect();
        assert!(leftovers.is_empty(), "temporary files must be cleaned");
        let _ = fs::remove_dir_all(dir);
    }
}
