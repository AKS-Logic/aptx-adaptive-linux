//! Just enough ELF32 little-endian to find symbols, map addresses and resolve pointer slots
//! (`spec/table-extraction.md` §2).

use std::collections::HashMap;

pub type Result<T> = std::result::Result<T, String>;

const ET_DYN: u16 = 3;
const EM_QDSP6: u16 = 164;
const EF_HEXAGON_MACH_V68: u32 = 0x68;
const PT_LOAD: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_RELA: u32 = 4;
const SHT_NOBITS: u32 = 8;
const SHT_DYNSYM: u32 = 11;
const SYM_SIZE: usize = 16;
const RELA_SIZE: usize = 12;
const R_HEX_32: u32 = 6;
const R_HEX_RELATIVE: u32 = 35;
pub const STT_FUNC: u8 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Sym {
    pub value: u32,
    pub size: u32,
    pub kind: u8,
}

struct Load {
    vaddr: u32,
    offset: u32,
    filesz: u32,
}

struct Rela {
    info: u32,
    addend: i32,
}

struct Section<'a> {
    name: &'a str,
    /// Empty for `SHT_NOBITS`.
    bytes: &'a [u8],
    kind: u32,
    offset: usize,
    link: usize,
    entsize: usize,
}

type Symbols<'a> = HashMap<&'a str, Sym>;

pub struct Elf<'a> {
    data: &'a [u8],
    loads: Vec<Load>,
    dynsym: Symbols<'a>,
    dynsym_by_index: Vec<Sym>,
    symtab: Symbols<'a>,
    rela: HashMap<u32, Rela>,
}

/// `base + index · stride`, or an error if a malformed file makes it overflow.
pub fn offset(base: usize, index: usize, stride: usize) -> Result<usize> {
    index
        .checked_mul(stride)
        .and_then(|d| base.checked_add(d))
        .ok_or_else(|| format!("offset {base:#x} + {index}·{stride} overflows"))
}

pub fn va_add(va: u32, delta: u32) -> Result<u32> {
    va.checked_add(delta)
        .ok_or_else(|| format!("address {va:#x} + {delta:#x} overflows"))
}

fn bytes(data: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    off.checked_add(len)
        .and_then(|end| data.get(off..end))
        .ok_or_else(|| format!("read of {len} bytes at file offset {off:#x} is out of bounds"))
}

/// Field reads at `base + field`, a header or entry offset plus a field offset.
fn u8_at(data: &[u8], base: usize, field: usize) -> Result<u8> {
    Ok(bytes(data, offset(base, field, 1)?, 1)?[0])
}

fn u16_at(data: &[u8], base: usize, field: usize) -> Result<u16> {
    let b = bytes(data, offset(base, field, 1)?, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(data: &[u8], base: usize, field: usize) -> Result<u32> {
    let b = bytes(data, offset(base, field, 1)?, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn usize_at(data: &[u8], base: usize, field: usize) -> Result<usize> {
    Ok(u32_at(data, base, field)? as usize)
}

fn cstr(data: &[u8], off: usize) -> Result<&str> {
    let rest = data
        .get(off..)
        .ok_or_else(|| format!("string at {off:#x} is out of bounds"))?;
    let end = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| format!("string at {off:#x} is unterminated"))?;
    std::str::from_utf8(&rest[..end]).map_err(|_| format!("string at {off:#x} is not UTF-8"))
}

fn check_header(data: &[u8]) -> Result<()> {
    let ident = bytes(data, 0, 16).map_err(|_| "file is too short to be ELF".to_string())?;
    if &ident[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if ident[4] != 1 || ident[5] != 1 {
        return Err("not a 32-bit little-endian ELF".into());
    }
    let e_type = u16_at(data, 0, 16)?;
    let machine = u16_at(data, 0, 18)?;
    let flags = u32_at(data, 0, 36)?;
    if e_type != ET_DYN || machine != EM_QDSP6 || flags != EF_HEXAGON_MACH_V68 {
        return Err(format!(
            "ELF header is e_type {e_type}, e_machine {machine}, e_flags {flags:#x}; \
             expected ET_DYN ({ET_DYN}), EM_QDSP6 ({EM_QDSP6}) and {EF_HEXAGON_MACH_V68:#x}"
        ));
    }
    Ok(())
}

fn loads(data: &[u8]) -> Result<Vec<Load>> {
    let phoff = usize_at(data, 0, 28)?;
    let phentsize = usize::from(u16_at(data, 0, 42)?);
    let mut loads = Vec::new();
    for i in 0..usize::from(u16_at(data, 0, 44)?) {
        let p = offset(phoff, i, phentsize)?;
        if u32_at(data, p, 0)? == PT_LOAD {
            loads.push(Load {
                offset: u32_at(data, p, 4)?,
                vaddr: u32_at(data, p, 8)?,
                filesz: u32_at(data, p, 16)?,
            });
        }
    }
    Ok(loads)
}

fn sections(data: &[u8]) -> Result<Vec<Section<'_>>> {
    let shoff = usize_at(data, 0, 32)?;
    let shentsize = usize::from(u16_at(data, 0, 46)?);
    let shnum = usize::from(u16_at(data, 0, 48)?);
    if shnum == 0 {
        return Ok(Vec::new());
    }
    let header = |i: usize| offset(shoff, i, shentsize);
    let shstrndx = usize::from(u16_at(data, 0, 50)?);
    if shstrndx >= shnum {
        return Err("section header string table index is out of range".into());
    }
    let shstr = usize_at(data, header(shstrndx)?, 16)?;
    (0..shnum)
        .map(|i| {
            let s = header(i)?;
            let kind = u32_at(data, s, 4)?;
            let sh_offset = usize_at(data, s, 16)?;
            let size = u32_at(data, s, 20)?;
            Ok(Section {
                name: cstr(data, offset(shstr, usize_at(data, s, 0)?, 1)?)?,
                bytes: if kind == SHT_NOBITS {
                    &[]
                } else {
                    bytes(data, sh_offset, size as usize)?
                },
                kind,
                offset: sh_offset,
                link: usize_at(data, s, 24)?,
                entsize: usize_at(data, s, 36)?,
            })
        })
        .collect()
}

fn symbols<'a>(
    data: &'a [u8],
    sections: &[Section],
    kind: u32,
) -> Result<Option<(Symbols<'a>, Vec<Sym>)>> {
    let Some(sec) = sections.iter().find(|s| s.kind == kind) else {
        return Ok(None);
    };
    let strtab = sections
        .get(sec.link)
        .ok_or("symbol table's string table index is out of range")?;
    let mut by_name = HashMap::new();
    let mut by_index = Vec::new();
    for i in 0..sec.bytes.len() / SYM_SIZE {
        let e = offset(sec.offset, i, SYM_SIZE)?;
        let sym = Sym {
            value: u32_at(data, e, 4)?,
            size: u32_at(data, e, 8)?,
            kind: u8_at(data, e, 12)? & 0xF,
        };
        by_index.push(sym);
        let shndx = u16_at(data, e, 14)?;
        let name = cstr(data, offset(strtab.offset, usize_at(data, e, 0)?, 1)?)?;
        if shndx != 0 && !name.is_empty() {
            by_name.entry(name).or_insert(sym);
        }
    }
    Ok(Some((by_name, by_index)))
}

fn rela_dyn(data: &[u8], sections: &[Section]) -> Result<HashMap<u32, Rela>> {
    let mut rela = HashMap::new();
    for sec in sections
        .iter()
        .filter(|s| s.kind == SHT_RELA && s.name == ".rela.dyn")
    {
        let entsize = if sec.entsize == 0 {
            RELA_SIZE
        } else {
            sec.entsize
        };
        for i in 0..sec.bytes.len() / entsize {
            let e = offset(sec.offset, i, entsize)?;
            rela.insert(
                u32_at(data, e, 0)?,
                Rela {
                    info: u32_at(data, e, 4)?,
                    addend: u32_at(data, e, 8)?.cast_signed(),
                },
            );
        }
    }
    Ok(rela)
}

impl<'a> Elf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        check_header(data)?;
        let sections = sections(data)?;
        let (dynsym, dynsym_by_index) =
            symbols(data, &sections, SHT_DYNSYM)?.ok_or("the file has no .dynsym")?;
        let symtab = symbols(data, &sections, SHT_SYMTAB)?
            .map(|(by_name, _)| by_name)
            .unwrap_or_default();
        Ok(Self {
            data,
            loads: loads(data)?,
            rela: rela_dyn(data, &sections)?,
            dynsym,
            dynsym_by_index,
            symtab,
        })
    }

    pub fn dynsym(&self, name: &str) -> Option<Sym> {
        self.dynsym.get(name).copied()
    }

    pub fn symtab(&self, name: &str) -> Option<Sym> {
        self.symtab.get(name).copied()
    }

    /// The `len` bytes at virtual address `va`, mapped through the `PT_LOAD` headers.
    pub fn bytes_at_va(&self, va: u32, len: usize) -> Result<&'a [u8]> {
        let end = u64::from(va) + len as u64;
        let load = self
            .loads
            .iter()
            .find(|l| va >= l.vaddr && end <= u64::from(l.vaddr) + u64::from(l.filesz))
            .ok_or_else(|| {
                format!("address {va:#x} (+{len}) is not in a PT_LOAD segment's file image")
            })?;
        let off = offset(load.offset as usize, (va - load.vaddr) as usize, 1)?;
        bytes(self.data, off, len)
    }

    pub fn u32_at_va(&self, va: u32) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.bytes_at_va(va, 4)?.try_into().unwrap(),
        ))
    }

    /// The address a 4-byte pointer slot at `va` holds after dynamic relocation.
    pub fn pointer(&self, va: u32) -> Result<u32> {
        match self.rela.get(&va) {
            Some(r) if r.info & 0xFF == R_HEX_32 => {
                let sym = self
                    .dynsym_by_index
                    .get((r.info >> 8) as usize)
                    .ok_or_else(|| format!("relocation at {va:#x} names a missing symbol"))?;
                sym.value
                    .checked_add_signed(r.addend)
                    .ok_or_else(|| format!("relocation at {va:#x} overflows"))
            }
            Some(r) if r.info & 0xFF == R_HEX_RELATIVE => Ok(r.addend.cast_unsigned()),
            _ => self.u32_at_va(va),
        }
    }
}
