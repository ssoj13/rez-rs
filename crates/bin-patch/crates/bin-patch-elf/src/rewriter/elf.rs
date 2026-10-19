// This file contains code derived from the `object-rewrite` crate.
// See THIRD-PARTY-LICENSES for license information.
// Source: https://github.com/gimli-rs/object/tree/master/crates/rewrite

use object::{
    build::{self},
    elf,
};

use super::{ElfError, Result};

enum BlockKind {
    FileHeader,
    ProgramHeaders,
    Segment,
    Section(build::elf::SectionId),
}

struct Block<'a> {
    #[allow(dead_code)]
    name: build::ByteString<'a>,
    kind: BlockKind,
    address: u64,
    size: u64,
    // Higher means better to move. 0 means never move.
    move_priority: u8,
}

/// Move sections between segments if needed, and assign file offsets to segments and sections.
pub(crate) fn move_sections(builder: &mut build::elf::Builder) -> Result<()> {
    builder.delete_orphans();
    builder.delete_unused_versions();
    builder.set_section_sizes();
    resize_in_place_load_segments(builder);

    let mut added_p_flags = Vec::new();
    let mut added_segments = 0;

    // Loop until we reach a fixed point for the number of additional segments needed.
    loop {
        let mut move_sections = find_move_sections(builder, added_segments)?;
        if move_sections.is_empty() {
            return Ok(());
        }

        // Calculate the unique PT_LOAD p_flags needed for moved sections.
        added_p_flags.clear();
        for id in &move_sections {
            let section = builder.sections.get_mut(*id);
            // Flag the section as needing to move.
            section.sh_offset = 0;
            // We need one PT_LOAD segment for each unique combination of p_flags.
            let p_flags = section.p_flags();
            if !added_p_flags.contains(&p_flags) {
                added_p_flags.push(p_flags);
            }
        }

        // Identify existing PT_LOAD segments that only contain movable sections in `move_sections`
        // and do not cover the ELF file header or program headers. These can be reused in-place.
        let reusable_load_segments = find_reusable_load_segments(builder, &move_sections);
        let new_load_segments = added_p_flags
            .len()
            .saturating_sub(reusable_load_segments.len());

        // If moving a section that is part of a non-PT_LOAD segment, then we may need to
        // split the segment, which will require an additional segment.
        let mut split_segments_count = 0;
        for segment in &builder.segments {
            if segment.p_type == elf::PT_LOAD {
                continue;
            }
            let mut any = false;
            let mut all = true;
            for id in &segment.sections {
                if move_sections.contains(id) {
                    any = true;
                } else {
                    all = false;
                }
            }
            if !any || all {
                continue;
            }
            // One new segment for the moved sections, plus any extra segments if the remaining
            // unmoved sections in this segment are discontiguous and cannot slide together.
            let remaining: Vec<_> = segment
                .sections
                .iter()
                .copied()
                .filter(|id| !move_sections.contains(id))
                .collect();
            let extra_discontiguous = count_discontiguous_splits(builder, &remaining);
            split_segments_count += 1 + extra_discontiguous;
        }

        // Check if we have reached a fixed point for the number of additional segments needed.
        let needed_segments = split_segments_count + new_load_segments;
        if added_segments < needed_segments {
            added_segments = needed_segments;
            continue;
        }

        // Record original addresses of moved sections so we can update DT_REL/DT_RELA/DT_JMPREL.
        let old_addresses: Vec<(build::elf::SectionId, u64)> = move_sections
            .iter()
            .map(|&id| (id, builder.sections.get(id).sh_addr))
            .collect();

        // Remove moved sections from any existing PT_LOAD segments that are being reused.
        for &seg_id in &reusable_load_segments {
            if let Some(seg) = builder.segments.iter_mut().find(|s| s.id() == seg_id) {
                seg.sections.clear();
                seg.p_filesz = 0;
                seg.p_memsz = 0;
            }
        }

        // Add or reuse the PT_LOAD segments and append sections to them.
        // Try to keep the same order of sections in the new segments.
        move_sections.sort_by_key(|id| {
            let section = builder.sections.get(*id);
            (section.sh_addr, section.sh_size)
        });
        let mut reusable_iter = reusable_load_segments.iter().copied();
        for p_flags in added_p_flags {
            let segment = if let Some(reused_id) = reusable_iter.next() {
                reinitialize_load_segment(
                    &mut builder.segments,
                    builder.load_align,
                    reused_id,
                    p_flags,
                )
            } else {
                builder
                    .segments
                    .add_load_segment(p_flags, builder.load_align)
            };
            for id in &move_sections {
                let section = builder.sections.get_mut(*id);
                if p_flags == section.p_flags() {
                    segment.append_section(section);
                }
            }
        }

        // Update dynamic relocation table addresses if any DynamicRelocation section moved.
        update_dynamic_relocation_addresses(builder, &old_addresses);

        // Split or move non-PT_LOAD segments that contain sections that have been moved.
        let mut split_segments = Vec::new();
        let segment_ids: Vec<_> = builder
            .segments
            .iter()
            .filter(|s| s.p_type != elf::PT_LOAD)
            .map(|s| s.id())
            .collect();

        for seg_id in segment_ids {
            let (any, all, remaining_sections, moved_in_seg) = {
                let segment = builder.segments.get(seg_id);
                let mut any = false;
                let mut all = true;
                let mut remaining = Vec::new();
                let mut moved = Vec::new();
                for &id in &segment.sections {
                    if move_sections.contains(&id) {
                        any = true;
                        moved.push(id);
                    } else {
                        all = false;
                        remaining.push(id);
                    }
                }
                (any, all, remaining, moved)
            };
            if !any {
                continue;
            }
            if !all {
                // Verify that the remaining sections are contiguous; if not, try to slide
                // movable sections down in memory/file, and split any remaining discontiguous runs.
                let contiguous_groups =
                    ensure_contiguous_or_split(builder, &remaining_sections, &move_sections);
                let segment = builder.segments.get_mut(seg_id);
                if let Some(first_group) = contiguous_groups.first() {
                    segment.sections = first_group.clone();
                } else {
                    segment.sections.clear();
                }
                for extra_group in contiguous_groups.into_iter().skip(1) {
                    split_segments.push((seg_id, extra_group, false));
                }
                split_segments.push((seg_id, moved_in_seg, true));
            }

            let sections = &builder.sections;
            builder
                .segments
                .get_mut(seg_id)
                .recalculate_ranges(sections);
        }

        // Add new segments due to splitting.
        for (segment_id, split_sections, append_moved) in split_segments {
            let segment = builder.segments.copy(segment_id);
            if append_moved {
                for id in split_sections {
                    let section = builder.sections.get_mut(id);
                    segment.append_section(section);
                }
            } else {
                segment.sections = split_sections;
                let sections = &builder.sections;
                segment.recalculate_ranges(sections);
            }
        }

        // Update the PT_PHDR segment to include the new program headers.
        let size = builder.program_headers_size();
        for segment in &mut builder.segments {
            if segment.p_type != elf::PT_PHDR {
                continue;
            }
            segment.p_filesz = size;
            segment.p_memsz = size;
        }
        return Ok(());
    }
}

/// Extend existing `PT_LOAD` segments in-place when their trailing section grows
/// and doing so does not overlap any subsequent section or segment in file or memory.
fn resize_in_place_load_segments(builder: &mut build::elf::Builder) {
    let interp = builder.interp_section();
    let load_ids: Vec<build::elf::SegmentId> = builder
        .segments
        .iter()
        .filter(|s| s.p_type == elf::PT_LOAD && s.p_filesz == s.p_memsz)
        .map(|s| s.id())
        .collect();

    for seg_id in load_ids {
        let seg = builder.segments.get(seg_id);
        let seg_vaddr = seg.p_vaddr;
        let seg_memsz = seg.p_memsz;
        let seg_offset = seg.p_offset;
        let seg_filesz = seg.p_filesz;
        let seg_sections = seg.sections.clone();

        // Find if any movable section in this segment now extends past `seg_vaddr + seg_memsz`.
        let mut needed_end_addr = seg_vaddr + seg_memsz;
        let mut needed_end_offset = seg_offset + seg_filesz;
        let mut grew_sections = Vec::new();

        for &sec_id in &seg_sections {
            let sec = builder.sections.get(sec_id);
            if !sec.is_alloc() || sec.sh_offset == 0 || sec.sh_type == elf::SHT_NOBITS {
                continue;
            }
            let sec_end_addr = sec.sh_addr.saturating_add(sec.sh_size);
            let sec_end_off = sec.sh_offset.saturating_add(sec.sh_size);
            if sec_end_addr > seg_vaddr + seg_memsz || sec_end_off > seg_offset + seg_filesz {
                if section_move_priority(sec, interp) == 0 {
                    grew_sections.clear();
                    break;
                }
                needed_end_addr = needed_end_addr.max(sec_end_addr);
                needed_end_offset = needed_end_offset.max(sec_end_off);
                grew_sections.push(sec_id);
            }
        }

        if grew_sections.is_empty() {
            continue;
        }

        // Verify no other section overlaps the grown section(s) in memory or file space.
        let mut conflict = false;
        for &sec_id in &grew_sections {
            let sec = builder.sections.get(sec_id);
            let s_addr_start = sec.sh_addr;
            let s_addr_end = sec.sh_addr.saturating_add(sec.sh_size);
            let s_off_start = sec.sh_offset;
            let s_off_end = sec.sh_offset.saturating_add(sec.sh_size);

            for other in &builder.sections {
                if other.id() == sec_id || other.sh_size == 0 {
                    continue;
                }
                if other.is_alloc()
                    && !(other.sh_type == elf::SHT_NOBITS && other.sh_flags.contains(elf::SHF_TLS))
                {
                    let o_start = other.sh_addr;
                    let o_end = other.sh_addr.saturating_add(other.sh_size);
                    if s_addr_start < o_end && o_start < s_addr_end {
                        conflict = true;
                        break;
                    }
                }
                if other.sh_type != elf::SHT_NOBITS && other.sh_offset != 0 {
                    let o_off_start = other.sh_offset;
                    let o_off_end = other.sh_offset.saturating_add(other.sh_size);
                    if s_off_start < o_off_end && o_off_start < s_off_end {
                        conflict = true;
                        break;
                    }
                }
            }
            if conflict {
                break;
            }
        }

        if conflict {
            continue;
        }

        // Verify no other PT_LOAD segment overlaps the extended range.
        for other_seg in &builder.segments {
            if other_seg.id() == seg_id || other_seg.p_type != elf::PT_LOAD {
                continue;
            }
            let align_mask = other_seg.p_align.saturating_sub(1);
            let other_vaddr_aligned = if other_seg.p_align.is_power_of_two() {
                other_seg.p_vaddr & !align_mask
            } else {
                other_seg.p_vaddr
            };
            let other_vend = other_seg.p_vaddr.saturating_add(other_seg.p_memsz);
            if seg_vaddr < other_vend && other_vaddr_aligned < needed_end_addr {
                conflict = true;
                break;
            }
            let other_fend = other_seg.p_offset.saturating_add(other_seg.p_filesz);
            if seg_offset < other_fend && other_seg.p_offset < needed_end_offset {
                conflict = true;
                break;
            }
        }

        if !conflict {
            let sections = &builder.sections;
            builder
                .segments
                .get_mut(seg_id)
                .recalculate_ranges(sections);
        }
    }
}

/// Find `PT_LOAD` segments whose sections are all in `move_sections` and that do not cover
/// the ELF file header or program header table.
fn find_reusable_load_segments(
    builder: &build::elf::Builder,
    move_sections: &[build::elf::SectionId],
) -> Vec<build::elf::SegmentId> {
    let e_phoff = builder.header.e_phoff;
    let mut reusable = Vec::new();
    for segment in &builder.segments {
        if segment.p_type != elf::PT_LOAD || segment.sections.is_empty() {
            continue;
        }
        let covers_file_header = segment.p_offset == 0 && segment.p_filesz > 0;
        let covers_phdr =
            segment.p_offset <= e_phoff && e_phoff < segment.p_offset + segment.p_filesz;
        if covers_file_header || covers_phdr {
            continue;
        }
        if segment.sections.iter().all(|id| move_sections.contains(id)) {
            reusable.push(segment.id());
        }
    }
    reusable
}

/// Reinitialize a reusable `PT_LOAD` segment at the end of the file and virtual address space.
fn reinitialize_load_segment<'a, 'b>(
    segments: &'a mut build::elf::Segments<'b>,
    align: u64,
    seg_id: build::elf::SegmentId,
    p_flags: elf::ProgramFlags,
) -> &'a mut build::elf::Segment<'b> {
    let mut max_offset = 0;
    let mut max_vaddr = 0;
    for seg in segments.iter() {
        if seg.p_type == elf::PT_LOAD && seg.id() != seg_id {
            max_offset = max_offset.max(seg.p_offset.saturating_add(seg.p_filesz));
            max_vaddr = max_vaddr.max(seg.p_vaddr.saturating_add(seg.p_memsz));
        }
    }
    let offset = align_up(max_offset, align);
    let vaddr = align_up(max_vaddr, align);
    let segment = segments.get_mut(seg_id);
    segment.p_flags = p_flags;
    segment.p_align = align;
    segment.p_offset = offset;
    segment.p_vaddr = vaddr;
    segment.p_paddr = vaddr;
    segment.p_filesz = 0;
    segment.p_memsz = 0;
    segment.sections.clear();
    segment
}

fn align_up(val: u64, align: u64) -> u64 {
    if align <= 1 {
        return val;
    }
    let rem = val % align;
    if rem == 0 {
        val
    } else {
        val.saturating_add(align - rem)
    }
}

/// Count how many extra segment splits are needed if `remaining` sections are discontiguous.
fn count_discontiguous_splits(
    builder: &build::elf::Builder,
    remaining: &[build::elf::SectionId],
) -> usize {
    if remaining.len() <= 1 {
        return 0;
    }
    let mut sorted = remaining.to_vec();
    sorted.sort_by_key(|&id| builder.sections.get(id).sh_addr);
    let mut splits = 0;
    for pair in sorted.windows(2) {
        let prev = builder.sections.get(pair[0]);
        let curr = builder.sections.get(pair[1]);
        let exp_addr = align_up(prev.sh_addr.saturating_add(prev.sh_size), curr.sh_addralign);
        let exp_off = align_up(
            prev.sh_offset.saturating_add(prev.sh_size),
            curr.sh_addralign,
        );
        if curr.sh_addr != exp_addr || curr.sh_offset != exp_off {
            splits += 1;
        }
    }
    splits
}

/// Ensure remaining sections in a non-`PT_LOAD` segment are contiguous by sliding movable sections
/// down when possible, or splitting into contiguous section groups when not.
fn ensure_contiguous_or_split(
    builder: &mut build::elf::Builder,
    remaining: &[build::elf::SectionId],
    move_sections: &[build::elf::SectionId],
) -> Vec<Vec<build::elf::SectionId>> {
    if remaining.is_empty() {
        return Vec::new();
    }
    let interp = builder.interp_section();
    let mut sorted = remaining.to_vec();
    sorted.sort_by_key(|&id| builder.sections.get(id).sh_addr);

    let mut groups: Vec<Vec<build::elf::SectionId>> = vec![vec![sorted[0]]];
    let mut slided_addrs = Vec::new();

    for &curr_id in &sorted[1..] {
        let prev_id = *groups.last().unwrap().last().unwrap();
        let (prev_addr, prev_size, prev_off) = {
            let prev = builder.sections.get(prev_id);
            (prev.sh_addr, prev.sh_size, prev.sh_offset)
        };
        let (curr_addr, curr_size, curr_off, curr_align, curr_prio) = {
            let curr = builder.sections.get(curr_id);
            (
                curr.sh_addr,
                curr.sh_size,
                curr.sh_offset,
                curr.sh_addralign,
                section_move_priority(curr, interp),
            )
        };

        let exp_addr = align_up(prev_addr.saturating_add(prev_size), curr_align);
        let exp_off = align_up(prev_off.saturating_add(prev_size), curr_align);

        if curr_addr == exp_addr && curr_off == exp_off {
            groups.last_mut().unwrap().push(curr_id);
            continue;
        }

        // Try sliding `curr_id` down to `(exp_addr, exp_off)` if it is movable and the target
        // range does not collide with any unmoved section.
        let can_slide = curr_prio > 0
            && exp_addr <= curr_addr
            && exp_off <= curr_off
            && !builder.sections.iter().any(|other| {
                if other.id() == curr_id
                    || move_sections.contains(&other.id())
                    || other.sh_size == 0
                {
                    return false;
                }
                let addr_overlap = other.is_alloc()
                    && exp_addr < other.sh_addr.saturating_add(other.sh_size)
                    && other.sh_addr < exp_addr.saturating_add(curr_size);
                let off_overlap = other.sh_type != elf::SHT_NOBITS
                    && other.sh_offset != 0
                    && exp_off < other.sh_offset.saturating_add(other.sh_size)
                    && other.sh_offset < exp_off.saturating_add(curr_size);
                addr_overlap || off_overlap
            });

        if can_slide {
            let sec = builder.sections.get_mut(curr_id);
            slided_addrs.push((curr_id, curr_addr));
            sec.sh_addr = exp_addr;
            sec.sh_offset = exp_off;
            groups.last_mut().unwrap().push(curr_id);
        } else {
            groups.push(vec![curr_id]);
        }
    }

    if !slided_addrs.is_empty() {
        update_dynamic_relocation_addresses(builder, &slided_addrs);
    }

    groups
}

/// Update `DT_REL`, `DT_RELA`, `DT_JMPREL`, and `DT_RELR` integer entries in `.dynamic` when a
/// `SectionData::DynamicRelocation` section is moved to a new virtual address.
fn update_dynamic_relocation_addresses(
    builder: &mut build::elf::Builder,
    old_addresses: &[(build::elf::SectionId, u64)],
) {
    let mut addr_remaps = Vec::new();
    for &(id, old_addr) in old_addresses {
        if old_addr == 0 {
            continue;
        }
        let section = builder.sections.get(id);
        if matches!(section.data, build::elf::SectionData::DynamicRelocation(_))
            && section.sh_addr != old_addr
        {
            addr_remaps.push((old_addr, section.sh_addr));
        }
    }
    if addr_remaps.is_empty() {
        return;
    }
    for section in &mut builder.sections {
        if let build::elf::SectionData::Dynamic(dynamic) = &mut section.data {
            for entry in dynamic.iter_mut() {
                if let build::elf::Dynamic::Integer { tag, val } = entry {
                    if matches!(
                        *tag,
                        elf::DT_REL | elf::DT_RELA | elf::DT_JMPREL | elf::DT_RELR
                    ) {
                        for &(old_addr, new_addr) in &addr_remaps {
                            if *val == old_addr {
                                *val = new_addr;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn section_move_priority(
    section: &build::elf::Section,
    interp: Option<build::elf::SectionId>,
) -> u8 {
    use build::elf::SectionData;
    match &section.data {
        // Can't move sections whose address may be referenced from
        // a section that we can't rewrite.
        SectionData::Data(_) => {
            if Some(section.id()) == interp {
                1
            } else {
                0
            }
        }
        SectionData::UninitializedData(_) | SectionData::Dynamic(_) => 0,
        // Dynamic relocation sections are referenced via DT_REL/DT_RELA/DT_JMPREL/DT_RELR
        // in `.dynamic`, which we update in `update_dynamic_relocation_addresses`.
        SectionData::DynamicRelocation(_)
        | SectionData::Relocation(_)
        | SectionData::Note(_)
        | SectionData::Attributes(_)
        | SectionData::SectionString
        | SectionData::Symbol
        | SectionData::SymbolSectionIndex
        | SectionData::String
        | SectionData::DynamicSymbol
        | SectionData::DynamicString
        | SectionData::Hash
        | SectionData::GnuHash
        | SectionData::GnuVersym
        | SectionData::GnuVerdef
        | SectionData::GnuVerneed => 2,
    }
}

pub(crate) fn find_move_sections(
    builder: &build::elf::Builder,
    added_segments: usize,
) -> Result<Vec<build::elf::SectionId>> {
    let mut move_sections = Vec::new();
    let mut blocks = Vec::new();
    let file_header_size = builder.file_header_size();
    let added_segments = u64::try_from(added_segments)
        .map_err(|_| ElfError::Modify("ELF segment count exceeds u64".into()))?;
    let program_headers_size = builder
        .encoder()
        .program_header_size()
        .checked_mul(added_segments)
        .and_then(|added| builder.program_headers_size().checked_add(added))
        .ok_or_else(|| ElfError::Modify("ELF program header table size overflow".into()))?;
    let interp = builder.interp_section();

    if let Some(segment) = builder.segments.find_load_segment_from_offset(0) {
        let address = segment.address_from_offset(0);
        blocks.push(Block {
            name: "file header".into(),
            kind: BlockKind::FileHeader,
            address,
            size: file_header_size,
            move_priority: 0,
        });
    }
    if let Some(segment) = builder
        .segments
        .find_load_segment_from_offset(builder.header.e_phoff)
    {
        let address = segment.address_from_offset(builder.header.e_phoff);
        blocks.push(Block {
            name: "program headers".into(),
            kind: BlockKind::ProgramHeaders,
            address,
            size: program_headers_size,
            move_priority: 0,
        });
    }
    for segment in &builder.segments {
        if segment.p_type != elf::PT_LOAD {
            continue;
        }
        // Add zero-sized blocks at the start and end of the segment
        // to prevent changing the segment address or size beyond safe in-place resizing.
        blocks.push(Block {
            name: "segment start".into(),
            kind: BlockKind::Segment,
            address: segment.p_vaddr,
            size: 0,
            move_priority: 0,
        });
        blocks.push(Block {
            name: "segment end".into(),
            kind: BlockKind::Segment,
            address: segment.p_vaddr + segment.p_memsz,
            size: 0,
            move_priority: 0,
        });
    }
    for section in &builder.sections {
        if !section.is_alloc() {
            continue;
        }
        // Note that it is allowed for SHT_NOBITS sections to have sh_offset == 0,
        // but we never move them, so we must be careful to skip them here.
        if section.sh_offset == 0 && section.sh_type != elf::SHT_NOBITS {
            // Newly added section that needs to be assigned to a segment,
            // or a section that has already been flagged for moving.
            move_sections.push(section.id());
            continue;
        }
        if section.sh_type == elf::SHT_NOBITS && section.sh_flags.contains(elf::SHF_TLS) {
            // Uninitialized TLS sections are not part of the address space.
            continue;
        }
        let move_priority = section_move_priority(section, interp);
        blocks.push(Block {
            name: (*section.name).into(),
            kind: BlockKind::Section(section.id()),
            address: section.sh_addr,
            size: section.sh_size,
            move_priority,
        });
    }
    blocks.sort_by_key(|block| (block.address, block.size));

    // For each pair of overlapping blocks, decide which one to move.
    let mut i = 0;
    while i + 1 < blocks.len() {
        let end_address = blocks[i].address + blocks[i].size;
        if end_address <= blocks[i + 1].address {
            i += 1;
            continue;
        }
        // Prefer moving the earlier block, since it is the reason for the overlap.
        if blocks[i].move_priority >= blocks[i + 1].move_priority {
            if blocks[i].move_priority == 0 {
                return Err(ElfError::Modify(
                    "Overlapping immovable sections".to_string(),
                ));
            }
            if let BlockKind::Section(section) = blocks[i].kind {
                move_sections.push(section);
                blocks.remove(i);
            } else {
                // Only sections can be moved.
                unreachable!();
            }
        } else if let BlockKind::Section(section) = blocks[i + 1].kind {
            move_sections.push(section);
            blocks.remove(i + 1);
        } else {
            // Only sections can be moved.
            unreachable!();
        }
    }
    Ok(move_sections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::Endianness;

    #[test]
    fn test_resize_in_place_load_segments_extends_trailing_movable_section() {
        let mut builder = build::elf::Builder::new(Endianness::Little, true);
        builder.load_align = 0x1000;

        let seg = builder.segments.add_load_segment(elf::PF_R, 0x1000);
        seg.p_offset = 0x1000;
        seg.p_vaddr = 0x1000;
        seg.p_paddr = 0x1000;
        seg.p_filesz = 0x20;
        seg.p_memsz = 0x20;
        let seg_id = seg.id();

        let sec = builder.sections.add();
        sec.name = ".note.custom".into();
        sec.sh_type = elf::SHT_NOTE;
        sec.sh_flags = elf::SHF_ALLOC;
        sec.sh_addr = 0x1000;
        sec.sh_offset = 0x1000;
        sec.sh_size = 0x40; // Grew from 0x20 to 0x40
        sec.sh_addralign = 4;
        sec.data = build::elf::SectionData::Note(vec![0u8; 0x40].into());
        let sec_id = sec.id();
        builder.segments.get_mut(seg_id).sections.push(sec_id);

        resize_in_place_load_segments(&mut builder);
        assert_eq!(builder.segments.get(seg_id).p_filesz, 0x40);
        assert_eq!(builder.segments.get(seg_id).p_memsz, 0x40);
        assert!(find_move_sections(&builder, 0).unwrap().is_empty());
    }

    #[test]
    fn test_dynamic_relocation_move_updates_dynamic_table_address_and_reuses_segment() {
        let mut builder = build::elf::Builder::new(Endianness::Little, true);
        builder.load_align = 0x1000;
        builder.header.e_phoff = 64;

        // First PT_LOAD covers file header at offset 0 and program headers at offset 64.
        let header_seg = builder.segments.add_load_segment(elf::PF_R, 0x1000);
        header_seg.p_offset = 0;
        header_seg.p_vaddr = 0;
        header_seg.p_paddr = 0;
        header_seg.p_filesz = 0x300;
        header_seg.p_memsz = 0x300;
        let header_seg_id = header_seg.id();

        let dyn_sec = builder.sections.add();
        dyn_sec.name = ".dynamic".into();
        dyn_sec.sh_type = elf::SHT_DYNAMIC;
        dyn_sec.sh_flags = elf::SHF_ALLOC | elf::SHF_WRITE;
        dyn_sec.sh_addr = 0x200;
        dyn_sec.sh_offset = 0x200;
        dyn_sec.sh_size = 0x20;
        dyn_sec.sh_addralign = 8;
        dyn_sec.data = build::elf::SectionData::Dynamic(vec![build::elf::Dynamic::Integer {
            tag: elf::DT_RELA,
            val: 0x2000,
        }]);
        let dyn_id = dyn_sec.id();
        builder
            .segments
            .get_mut(header_seg_id)
            .sections
            .push(dyn_id);

        // Second PT_LOAD contains only a movable DynamicRelocation section that needs moving.
        let rel_seg = builder.segments.add_load_segment(elf::PF_R, 0x1000);
        rel_seg.p_offset = 0x2000;
        rel_seg.p_vaddr = 0x2000;
        rel_seg.p_paddr = 0x2000;
        rel_seg.p_filesz = 0x18;
        rel_seg.p_memsz = 0x18;
        let rel_seg_id = rel_seg.id();

        let rela_sec = builder.sections.add();
        rela_sec.name = ".rela.dyn".into();
        rela_sec.sh_type = elf::SHT_RELA;
        rela_sec.sh_flags = elf::SHF_ALLOC;
        rela_sec.sh_addr = 0x2000;
        rela_sec.sh_offset = 0; // Flagged to move
        rela_sec.sh_size = 0x18;
        rela_sec.sh_addralign = 8;
        rela_sec.data = build::elf::SectionData::DynamicRelocation(vec![build::elf::Relocation {
            r_offset: 0x3000,
            symbol: None,
            r_type: elf::R_X86_64_RELATIVE,
            r_addend: 0,
        }]);
        let rela_id = rela_sec.id();
        builder.segments.get_mut(rel_seg_id).sections.push(rela_id);

        let initial_load_count = builder
            .segments
            .iter()
            .filter(|s| s.p_type == elf::PT_LOAD)
            .count();
        move_sections(&mut builder).unwrap();
        let final_load_count = builder
            .segments
            .iter()
            .filter(|s| s.p_type == elf::PT_LOAD)
            .count();

        // Reused the existing movable-only PT_LOAD segment rather than allocating an extra one.
        assert_eq!(initial_load_count, final_load_count);

        let new_rela_addr = builder.sections.get(rela_id).sh_addr;
        assert_eq!(new_rela_addr, 0x1000);
        let build::elf::SectionData::Dynamic(dynamic) = &builder.sections.get(dyn_id).data else {
            panic!("expected Dynamic section data");
        };
        assert_eq!(
            dynamic[0],
            build::elf::Dynamic::Integer {
                tag: elf::DT_RELA,
                val: new_rela_addr,
            }
        );
    }

    #[test]
    fn test_ensure_contiguous_slides_movable_sections_together() {
        let mut builder = build::elf::Builder::new(Endianness::Little, true);

        let s1 = builder.sections.add();
        s1.name = ".note.1".into();
        s1.sh_type = elf::SHT_NOTE;
        s1.sh_flags = elf::SHF_ALLOC;
        s1.sh_addr = 0x1000;
        s1.sh_offset = 0x1000;
        s1.sh_size = 0x10;
        s1.sh_addralign = 4;
        s1.data = build::elf::SectionData::Note(vec![0u8; 0x10].into());
        let s1_id = s1.id();

        let s2 = builder.sections.add();
        s2.name = ".note.2".into();
        s2.sh_type = elf::SHT_NOTE;
        s2.sh_flags = elf::SHF_ALLOC;
        s2.sh_addr = 0x1020; // Gap of 0x10 where a moved section used to be
        s2.sh_offset = 0x1020;
        s2.sh_size = 0x10;
        s2.sh_addralign = 4;
        s2.data = build::elf::SectionData::Note(vec![0u8; 0x10].into());
        let s2_id = s2.id();

        let groups = ensure_contiguous_or_split(&mut builder, &[s1_id, s2_id], &[]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![s1_id, s2_id]);
        assert_eq!(builder.sections.get(s2_id).sh_addr, 0x1010);
        assert_eq!(builder.sections.get(s2_id).sh_offset, 0x1010);
    }
}
