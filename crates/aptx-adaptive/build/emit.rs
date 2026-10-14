//! Writes the canonical tables as the consts `src/codec/tables.rs` includes.

use std::fmt::Display;
use std::fmt::Write as _;

use crate::extract::{ARRAYS, CODEWORD_TABLES, Elem, Tables};

fn list<T: Display>(v: impl IntoIterator<Item = T>) -> String {
    let items: Vec<String> = v.into_iter().map(|x| x.to_string()).collect();
    format!("[{}]", items.join(", "))
}

fn joined(v: impl IntoIterator<Item = String>) -> String {
    v.into_iter().collect::<Vec<_>>().join(", ")
}

fn signed(v: &[u32]) -> String {
    list(v.iter().map(|&x| x.cast_signed()))
}

fn codes(v: &[u32]) -> String {
    list(
        v.as_chunks::<2>()
            .0
            .iter()
            .map(|[code, len]| format!("c({code}, {len})")),
    )
}

struct Out<'a> {
    t: &'a Tables,
    rust: String,
}

impl Out<'_> {
    fn get(&self, id: &str) -> &[u32] {
        &self.t[id]
    }

    fn item(&mut self, doc: &str, decl: impl Display) {
        if !doc.is_empty() {
            let _ = writeln!(self.rust, "/// {doc}");
        }
        let _ = writeln!(self.rust, "pub const {decl};");
    }
}

pub fn rust(t: &Tables) -> String {
    let mut o = Out {
        t,
        rust: String::new(),
    };
    arrays(&mut o);
    filterbank(&mut o);
    alloc_huffman(&mut o);
    codewords(&mut o);
    o.rust
}

fn arrays(o: &mut Out) {
    for a in ARRAYS {
        let Some(name) = a.rust else { continue };
        let v = o.get(a.id);
        let values = if matches!(a.elem, Elem::I8 | Elem::I16 | Elem::I32) {
            signed(v)
        } else {
            list(v)
        };
        o.item(
            "",
            format!("{name}: [{}; {}] = {values}", a.elem.rust(), v.len()),
        );
    }
}

fn filterbank(o: &mut Out) {
    o.item(
        "K1..K7",
        format!(
            "DCT: [i32; 7] = {}",
            signed(o.get("filterbank_8band_modulation.dct"))
        ),
    );
    let frontend = o
        .get("filterbank_8band_modulation.frontend")
        .as_chunks::<8>()
        .0
        .iter()
        .map(|&[a, b, c, d, diff_out, diff_const, sum_out, sum_const]| {
            format!(
                "Frontend {{ add: [{a}, {b}], sub: [{c}, {d}], diff_out: {diff_out}, \
                 diff_const: {diff_const}, sum_out: {sum_out}, sum_const: {sum_const} }}"
            )
        });
    o.item(
        "",
        format!("FRONTEND: [Frontend; 4] = [{}]", joined(frontend)),
    );

    let quant = (2..=9).map(|bt| {
        let q = |k: &str| signed(o.get(&format!("quantiser.tables.{bt}.{k}")));
        format!(
            "QuantTable {{ threshold: &{}, dither: &{}, incr: &{} }}",
            q("threshold"),
            q("dither"),
            q("logdelta_incr")
        )
    });
    o.item(
        "Indexed by table size Bt − 2.",
        format!("QUANT: [QuantTable; 8] = [{}]", joined(quant)),
    );
    o.item(
        "",
        format!(
            "TAPS_4BAND: [usize; 4] = {}",
            list(o.get("predictor_taps.4band"))
        ),
    );
}

fn alloc_huffman(o: &mut Out) {
    for (name, id) in [("ABS_A", "table_a"), ("ABS_B", "table_b")] {
        let v = o.get(&format!("alloc_absolute_huffman.{id}"));
        o.item(
            "",
            format!("{name}: [Code; {}] = {}", v.len() / 2, codes(v)),
        );
    }
    for bands in [8, 4] {
        let v = o.get(&format!("alloc_absolute_huffman.pair_table_{bands}band"));
        o.item(
            "true = table B",
            format!(
                "PAIR_TABLE_{bands}BAND: [bool; 8] = {}",
                list(v.iter().map(|&b| b == 1))
            ),
        );
    }
    let diff = o.get("alloc_diff_huffman.tables").chunks(32).map(codes);
    o.item("", format!("DIFF: [[Code; 16]; 9] = [{}]", joined(diff)));
    o.item(
        "",
        format!(
            "DIFF_INDEX: [usize; 16] = {}",
            list(o.get("alloc_diff_huffman.table_index"))
        ),
    );
    let ranges = o
        .get("alloc_diff_huffman.range")
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[lo, hi]| format!("({}, {})", lo.cast_signed(), hi.cast_signed()));
    o.item(
        "",
        format!("DIFF_RANGE: [(i32, i32); 16] = {}", list(ranges)),
    );
}

fn codewords(o: &mut Out) {
    for table in &CODEWORD_TABLES {
        let v = o.get(table.id);
        o.item(
            "",
            format!("{}: [Code; {}] = {}", table.rust, v.len() / 2, codes(v)),
        );
    }

    let codeword = o
        .get("codeword_huffman.entries")
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[table, pair_shift, raw_lsbs, msb_mask]| {
            let table = match table {
                0 => "&[]".to_string(),
                n => format!("&T{n}"),
            };
            format!(
                "CodewordEntry {{ table: {table}, pair_shift: {pair_shift}, \
                 raw_lsbs: {raw_lsbs}, msb_mask: {msb_mask} }}"
            )
        });
    o.item(
        "Indexed by allocation 0..=16.",
        format!("CODEWORD: [CodewordEntry; 17] = [{}]", joined(codeword)),
    );
}
