use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(crate) struct StringOffset(usize);

#[derive(Clone, Copy)]
pub(crate) struct VectorOffset(usize);

#[derive(Clone, Copy)]
pub(crate) struct TableMarker;

#[derive(Clone, Copy)]
pub(crate) struct TableOffset;

pub(crate) trait SlotValue {
    fn store(self, builder: &mut FlatBufferBuilder, slot: u16);
}

impl SlotValue for StringOffset {
    fn store(self, builder: &mut FlatBufferBuilder, slot: u16) {
        builder.string_slots.insert(slot, self.0);
    }
}

impl SlotValue for VectorOffset {
    fn store(self, builder: &mut FlatBufferBuilder, slot: u16) {
        builder.vector_slots.insert(slot, self.0);
    }
}

pub(crate) struct FlatBufferBuilder {
    strings: Vec<String>,
    vectors: Vec<Vec<usize>>,
    string_slots: BTreeMap<u16, usize>,
    vector_slots: BTreeMap<u16, usize>,
    bool_slots: BTreeMap<u16, bool>,
    finished: Vec<u8>,
}

impl FlatBufferBuilder {
    pub(crate) fn new() -> Self {
        Self {
            strings: Vec::new(),
            vectors: Vec::new(),
            string_slots: BTreeMap::new(),
            vector_slots: BTreeMap::new(),
            bool_slots: BTreeMap::new(),
            finished: Vec::new(),
        }
    }

    pub(crate) fn create_string(&mut self, value: &str) -> StringOffset {
        let index = self.strings.len();
        self.strings.push(value.to_owned());
        StringOffset(index)
    }

    pub(crate) fn create_vector(&mut self, values: &[StringOffset]) -> VectorOffset {
        let index = self.vectors.len();
        self.vectors
            .push(values.iter().map(|value| value.0).collect());
        VectorOffset(index)
    }

    pub(crate) fn start_table(&mut self) -> TableMarker {
        TableMarker
    }

    pub(crate) fn push_slot_always<T: SlotValue>(&mut self, slot: u16, value: T) {
        value.store(self, slot);
    }

    pub(crate) fn push_slot(&mut self, slot: u16, value: bool, default: bool) {
        if value != default {
            self.bool_slots.insert(slot, value);
        }
    }

    pub(crate) fn end_table(&mut self, _marker: TableMarker) -> TableOffset {
        TableOffset
    }

    pub(crate) fn finish(&mut self, _table: TableOffset, identifier: Option<&str>) {
        let version = self.string_slots.get(&4).copied();
        let read_write = self.vector_slots.get(&18).copied();
        let read_only = self.vector_slots.get(&20).copied();
        self.finished = encode(
            identifier.unwrap_or("SBOX"),
            version.map(|index| self.strings[index].as_str()),
            read_write.map(|index| self.vectors[index].as_slice()),
            read_only.map(|index| self.vectors[index].as_slice()),
            &self.strings,
            self.bool_slots.get(&6).copied().unwrap_or(false),
            self.bool_slots.get(&10).copied().unwrap_or(false),
            self.bool_slots.get(&14).copied().unwrap_or(false),
        );
    }

    pub(crate) fn finished_data(&self) -> &[u8] {
        &self.finished
    }
}

#[allow(clippy::too_many_arguments)]
fn encode(
    identifier: &str,
    version: Option<&str>,
    read_write: Option<&[usize]>,
    read_only: Option<&[usize]>,
    strings: &[String],
    app_container: bool,
    disallow_win32k: bool,
    least_privilege: bool,
) -> Vec<u8> {
    const VTABLE_START: usize = 8;
    const TABLE_START: usize = 32;
    const TABLE_SIZE: usize = 20;
    let mut bytes = vec![0u8; TABLE_START + TABLE_SIZE];
    write_u32(&mut bytes, 0, TABLE_START as u32);
    let id = identifier.as_bytes();
    bytes[4..8].copy_from_slice(id.get(..4).unwrap_or(b"SBOX"));

    write_u16(&mut bytes, VTABLE_START, 22);
    write_u16(&mut bytes, VTABLE_START + 2, TABLE_SIZE as u16);
    for (index, offset) in [4u16, 8, 0, 9, 0, 10, 0, 12, 16].into_iter().enumerate() {
        write_u16(&mut bytes, VTABLE_START + 4 + index * 2, offset);
    }
    write_i32(&mut bytes, TABLE_START, (TABLE_START - VTABLE_START) as i32);
    bytes[TABLE_START + 8] = u8::from(app_container);
    bytes[TABLE_START + 9] = u8::from(disallow_win32k);
    bytes[TABLE_START + 10] = u8::from(least_privilege);

    if let Some(value) = version {
        let target = append_string(&mut bytes, value);
        patch_offset(&mut bytes, TABLE_START + 4, target);
    }
    if let Some(values) = read_write {
        let target = append_string_vector(&mut bytes, values, strings);
        patch_offset(&mut bytes, TABLE_START + 12, target);
    }
    if let Some(values) = read_only {
        let target = append_string_vector(&mut bytes, values, strings);
        patch_offset(&mut bytes, TABLE_START + 16, target);
    }
    bytes
}

fn append_string_vector(bytes: &mut Vec<u8>, values: &[usize], strings: &[String]) -> usize {
    align(bytes, 4);
    let vector_start = bytes.len();
    bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
    let elements_start = bytes.len();
    bytes.resize(elements_start + values.len() * 4, 0);
    for (index, string_index) in values.iter().copied().enumerate() {
        let target = append_string(bytes, &strings[string_index]);
        patch_offset(bytes, elements_start + index * 4, target);
    }
    vector_start
}

fn append_string(bytes: &mut Vec<u8>, value: &str) -> usize {
    align(bytes, 4);
    let start = bytes.len();
    bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
    start
}

fn patch_offset(bytes: &mut [u8], field: usize, target: usize) {
    write_u32(bytes, field, (target - field) as u32);
}

fn align(bytes: &mut Vec<u8>, alignment: usize) {
    let padding = (alignment - bytes.len() % alignment) % alignment;
    bytes.resize(bytes.len() + padding, 0);
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    struct SandboxSpecification<'a> {
        table: flatbuffers_parser::Table<'a>,
    }

    impl<'a> flatbuffers_parser::Follow<'a> for SandboxSpecification<'a> {
        type Inner = Self;

        unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
            Self {
                // SAFETY: the real FlatBuffers verifier runs before this accessor is returned.
                table: unsafe { flatbuffers_parser::Table::new(buf, loc) },
            }
        }
    }

    impl flatbuffers_parser::Verifiable for SandboxSpecification<'_> {
        fn run_verifier(
            verifier: &mut flatbuffers_parser::Verifier<'_, '_>,
            position: usize,
        ) -> Result<(), flatbuffers_parser::InvalidFlatbuffer> {
            verifier
                .visit_table(position)?
                .visit_field::<flatbuffers_parser::ForwardsUOffset<&str>>("version", 4, true)?
                .visit_field::<bool>("app_container", 6, true)?
                .visit_field::<bool>("disallow_win32k", 10, true)?
                .visit_field::<bool>("least_privilege", 14, true)?
                .visit_field::<flatbuffers_parser::ForwardsUOffset<
                    flatbuffers_parser::Vector<'_, flatbuffers_parser::ForwardsUOffset<&str>>,
                >>("read_write", 18, true)?
                .visit_field::<flatbuffers_parser::ForwardsUOffset<
                    flatbuffers_parser::Vector<'_, flatbuffers_parser::ForwardsUOffset<&str>>,
                >>("read_only", 20, true)?
                .finish();
            Ok(())
        }
    }

    impl<'a> SandboxSpecification<'a> {
        fn version(&self) -> &'a str {
            // SAFETY: this accessor is used only after root verification.
            unsafe {
                self.table
                    .get::<flatbuffers_parser::ForwardsUOffset<&'a str>>(4, None)
                    .expect("required version")
            }
        }

        fn app_container(&self) -> bool {
            // SAFETY: this accessor is used only after root verification.
            unsafe { self.table.get::<bool>(6, Some(false)).unwrap() }
        }

        fn disallow_win32k(&self) -> bool {
            // SAFETY: this accessor is used only after root verification.
            unsafe { self.table.get::<bool>(10, Some(false)).unwrap() }
        }

        fn least_privilege(&self) -> bool {
            // SAFETY: this accessor is used only after root verification.
            unsafe { self.table.get::<bool>(14, Some(false)).unwrap() }
        }

        fn read_write(
            &self,
        ) -> flatbuffers_parser::Vector<'a, flatbuffers_parser::ForwardsUOffset<&'a str>> {
            // SAFETY: this accessor is used only after root verification.
            unsafe {
                self.table
                    .get::<flatbuffers_parser::ForwardsUOffset<
                        flatbuffers_parser::Vector<
                            'a,
                            flatbuffers_parser::ForwardsUOffset<&'a str>,
                        >,
                    >>(18, None)
                    .expect("required read-write paths")
            }
        }

        fn read_only(
            &self,
        ) -> flatbuffers_parser::Vector<'a, flatbuffers_parser::ForwardsUOffset<&'a str>> {
            // SAFETY: this accessor is used only after root verification.
            unsafe {
                self.table
                    .get::<flatbuffers_parser::ForwardsUOffset<
                        flatbuffers_parser::Vector<
                            'a,
                            flatbuffers_parser::ForwardsUOffset<&'a str>,
                        >,
                    >>(20, None)
                    .expect("required read-only paths")
            }
        }
    }

    #[test]
    fn emits_flatbuffer_header_and_identifier() {
        let mut builder = FlatBufferBuilder::new();
        let version = builder.create_string("0.1.0");
        let path = builder.create_string(r"C:\repo");
        let paths = builder.create_vector(&[path]);
        let table = builder.start_table();
        builder.push_slot_always(4, version);
        builder.push_slot(6, true, false);
        builder.push_slot_always(18, paths);
        let table = builder.end_table(table);
        builder.finish(table, Some("SBOX"));
        assert_eq!(&builder.finished_data()[4..8], b"SBOX");
        assert_eq!(
            u32::from_le_bytes(builder.finished_data()[0..4].try_into().unwrap()),
            32
        );
    }

    #[test]
    fn real_flatbuffers_parser_round_trips_containment_fields() {
        let mut builder = FlatBufferBuilder::new();
        let version = builder.create_string("0.1.0");
        let repository = builder.create_string(r"C:\repo");
        let toolchain = builder.create_string(r"C:\toolchain");
        let read_write = builder.create_vector(&[repository]);
        let read_only = builder.create_vector(&[toolchain]);
        let table = builder.start_table();
        builder.push_slot_always(4, version);
        builder.push_slot(6, true, false);
        builder.push_slot(10, true, false);
        builder.push_slot(14, true, false);
        builder.push_slot_always(18, read_write);
        builder.push_slot_always(20, read_only);
        let table = builder.end_table(table);
        builder.finish(table, Some("SBOX"));

        let bytes = builder.finished_data();
        assert!(flatbuffers_parser::buffer_has_identifier(
            bytes, "SBOX", false
        ));
        let parsed = flatbuffers_parser::root::<SandboxSpecification<'_>>(bytes)
            .expect("containment specification must be a valid FlatBuffer");
        assert_eq!(parsed.version(), "0.1.0");
        assert!(parsed.app_container());
        assert!(parsed.disallow_win32k());
        assert!(parsed.least_privilege());
        assert_eq!(
            parsed.read_write().iter().collect::<Vec<_>>(),
            vec![r"C:\repo"]
        );
        assert_eq!(
            parsed.read_only().iter().collect::<Vec<_>>(),
            vec![r"C:\toolchain"]
        );
    }
}
