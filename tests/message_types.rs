pub use bincode::{Decode, Encode};
use rustcomm::prelude::*;
use std::collections::HashMap;

// ============================================================================
// SimpleMessage - Simple struct with primitives
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct SimpleMessage {
    pub id: u32,
    pub value: i32,
    pub flag: bool,
}
impl_message!(SimpleMessage);

// ============================================================================
// ByteVector - Basic vector of bytes
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct ByteVector {
    pub data: Vec<u8>,
}
impl_message!(ByteVector);

// ============================================================================
// TextMessage - Struct with String
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct TextMessage {
    pub text: String,
    pub count: u16,
}
impl_message!(TextMessage);

// ============================================================================
// OptionalMessage - Struct with Option types
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct OptionalMessage {
    pub required: u32,
    pub optional_int: Option<i32>,
    pub optional_string: Option<String>,
}
impl_message!(OptionalMessage);

// ============================================================================
// ListMessage - Struct with Vec
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct ListMessage {
    pub items: Vec<u32>,
    pub names: Vec<String>,
}
impl_message!(ListMessage);

// ============================================================================
// Point & NestedMessage - Nested struct
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct NestedMessage {
    pub id: u32,
    pub position: Point,
    pub waypoints: Vec<Point>,
}
impl_message!(NestedMessage);

// ============================================================================
// EnumMessage - Enum message
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub enum Action {
    Move { x: f32, y: f32 },
    Attack { target_id: u32 },
    Idle,
}

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct EnumMessage {
    pub player_id: u32,
    pub action: Action,
}
impl_message!(EnumMessage);

// ============================================================================
// TupleMessage - Tuple struct
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct TupleMessage(pub u32, pub String, pub bool);
impl_message!(TupleMessage);

// ============================================================================
// NumericMessage - Struct with all numeric types
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct NumericMessage {
    pub u8_val: u8,
    pub u16_val: u16,
    pub u32_val: u32,
    pub u64_val: u64,
    pub i8_val: i8,
    pub i16_val: i16,
    pub i32_val: i32,
    pub i64_val: i64,
    pub f32_val: f32,
    pub f64_val: f64,
}
impl_message!(NumericMessage);

// ============================================================================
// EmptyMessage - Empty struct (unit-like)
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct EmptyMessage;
impl_message!(EmptyMessage);

// ============================================================================
// ComplexMessage - Struct with nested Option<Vec<T>>
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct ComplexMessage {
    pub id: u32,
    pub tags: Option<Vec<String>>,
    pub scores: Option<Vec<i32>>,
}
impl_message!(ComplexMessage);

// ============================================================================
// HashMapMessage - HashMap message
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct HashMapMessage {
    pub player_scores: HashMap<String, u32>,
    pub config: HashMap<String, String>,
}
impl_message!(HashMapMessage);

// ============================================================================
// NestedHashMapMessage - Nested HashMap with complex values
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct NestedHashMapMessage {
    pub player_data: HashMap<u32, Point>,
    pub tags_per_player: HashMap<u32, Vec<String>>,
}
impl_message!(NestedHashMapMessage);

// ============================================================================
// CompositeMessage - Message containing other messages
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct CompositeMessage {
    pub id: u32,
    pub simple: SimpleMessage,
    pub optional_text: Option<TextMessage>,
    pub nested: NestedMessage,
}
impl_message!(CompositeMessage);

// ============================================================================
// BatchMessage - Message with Vec of messages
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct BatchMessage {
    pub batch_id: u32,
    pub simple_messages: Vec<SimpleMessage>,
    pub points: Vec<Point>,
}
impl_message!(BatchMessage);

// ============================================================================
// MessageMapMessage - Message with HashMap of messages
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct MessageMapMessage {
    pub map_id: u32,
    pub messages_by_id: HashMap<u32, SimpleMessage>,
    pub positions_by_player: HashMap<String, Point>,
}
impl_message!(MessageMapMessage);

// ============================================================================
// UltraNestedMessage - Complex message with multiple levels of nesting
// ============================================================================

#[derive(Encode, Decode, Debug, Clone, PartialEq)]
pub struct UltraNestedMessage {
    pub id: u32,
    pub batch: BatchMessage,
    pub composite_map: HashMap<String, CompositeMessage>,
    pub optional_batches: Option<Vec<BatchMessage>>,
}
impl_message!(UltraNestedMessage);
