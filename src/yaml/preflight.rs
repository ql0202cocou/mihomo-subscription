use std::{ffi::CStr, marker::PhantomData, mem::MaybeUninit};

use unsafe_libyaml_norway as unsafe_yaml;

use super::{take, YamlError, MAX_BYTES, MAX_DEPTH};

const MAX_LOADER_EVENTS: usize = 20_000;

pub(super) fn check(input: &str) -> Result<(), YamlError> {
    let mut parser = Parser::new(input.as_bytes())?;
    let mut documents = 0;
    let mut depth = 0;
    let mut events = MAX_LOADER_EVENTS;
    let mut bytes = MAX_BYTES;

    loop {
        let event = parser.next()?;
        match event.0.type_ {
            unsafe_yaml::YAML_STREAM_START_EVENT | unsafe_yaml::YAML_DOCUMENT_END_EVENT => {}
            unsafe_yaml::YAML_STREAM_END_EVENT => return Ok(()),
            unsafe_yaml::YAML_DOCUMENT_START_EVENT => {
                documents += 1;
                if documents > 1 {
                    return Err(YamlError::TooComplex);
                }
            }
            unsafe_yaml::YAML_ALIAS_EVENT => {
                take(&mut events, 1)?;
                check_depth(depth + 1)?;
                // The alias anchor is valid for as long as the event is alive.
                let alias = unsafe { event.0.data.alias };
                take(&mut bytes, string_len(alias.anchor))?;
            }
            unsafe_yaml::YAML_SCALAR_EVENT => {
                take(&mut events, 1)?;
                check_depth(depth + 1)?;
                // The scalar fields are valid for as long as the event is alive.
                let scalar = unsafe { event.0.data.scalar };
                let length = usize::try_from(scalar.length).map_err(|_| YamlError::TooComplex)?;
                take(&mut bytes, length)?;
                take(&mut bytes, string_len(scalar.tag))?;
                take(&mut bytes, string_len(scalar.anchor))?;
            }
            kind @ (unsafe_yaml::YAML_SEQUENCE_START_EVENT
            | unsafe_yaml::YAML_MAPPING_START_EVENT) => {
                take(&mut events, 1)?;
                depth += 1;
                check_depth(depth)?;
                // The collection fields are valid for as long as the event is alive.
                let (tag, anchor) = if kind == unsafe_yaml::YAML_SEQUENCE_START_EVENT {
                    let sequence = unsafe { event.0.data.sequence_start };
                    (sequence.tag, sequence.anchor)
                } else {
                    let mapping = unsafe { event.0.data.mapping_start };
                    (mapping.tag, mapping.anchor)
                };
                take(&mut bytes, string_len(tag))?;
                take(&mut bytes, string_len(anchor))?;
            }
            unsafe_yaml::YAML_SEQUENCE_END_EVENT | unsafe_yaml::YAML_MAPPING_END_EVENT => {
                take(&mut events, 1)?;
                depth = depth.checked_sub(1).ok_or(YamlError::Parse)?;
            }
            _ => return Err(YamlError::Parse),
        }
    }
}

/// 节点所在深度(标量与别名按其父集合深度加一计)不得超过 [`MAX_DEPTH`]。
fn check_depth(depth: usize) -> Result<(), YamlError> {
    if depth > MAX_DEPTH {
        Err(YamlError::TooComplex)
    } else {
        Ok(())
    }
}

fn string_len(value: *const u8) -> usize {
    if value.is_null() {
        return 0;
    }
    // libyaml owns a NUL-terminated string here; the event releases it afterwards.
    unsafe { CStr::from_ptr(value.cast()) }.to_bytes().len()
}

struct Parser<'input> {
    inner: Box<unsafe_yaml::yaml_parser_t>,
    _input: PhantomData<&'input [u8]>,
}

impl<'input> Parser<'input> {
    fn new(input: &'input [u8]) -> Result<Self, YamlError> {
        let mut inner = Box::<unsafe_yaml::yaml_parser_t>::new_uninit();
        let ptr = inner.as_mut_ptr();
        // The boxed parser has a stable address when libyaml stores its self-pointer.
        let inner = unsafe {
            if unsafe_yaml::yaml_parser_initialize(ptr).fail {
                unsafe_yaml::yaml_parser_delete(ptr);
                return Err(YamlError::Parse);
            }
            unsafe_yaml::yaml_parser_set_encoding(ptr, unsafe_yaml::YAML_UTF8_ENCODING);
            unsafe_yaml::yaml_parser_set_input_string(ptr, input.as_ptr(), input.len() as u64);
            inner.assume_init()
        };
        Ok(Self {
            inner,
            _input: PhantomData,
        })
    }

    fn next(&mut self) -> Result<Event, YamlError> {
        let mut event = Event::new();
        // Both objects remain valid and pinned for the duration of the call.
        if unsafe { unsafe_yaml::yaml_parser_parse(self.inner.as_mut(), &mut event.0) }.fail {
            return Err(YamlError::Parse);
        }
        Ok(event)
    }
}

impl Drop for Parser<'_> {
    fn drop(&mut self) {
        // The parser was successfully initialized and no event borrows it.
        unsafe { unsafe_yaml::yaml_parser_delete(self.inner.as_mut()) }
    }
}

struct Event(unsafe_yaml::yaml_event_t);

impl Event {
    fn new() -> Self {
        // A zeroed libyaml event is YAML_NO_EVENT and owns no allocations.
        Self(unsafe { MaybeUninit::<unsafe_yaml::yaml_event_t>::zeroed().assume_init() })
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // Every parse call initializes the event, including its failure paths.
        unsafe { unsafe_yaml::yaml_event_delete(&mut self.0) }
    }
}
