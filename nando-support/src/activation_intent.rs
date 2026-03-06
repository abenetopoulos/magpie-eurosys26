use std::collections::HashSet;
use std::fmt::Debug;

use serde::{Deserialize, Serialize};
use serde_json::value::Value as SerdeJsonValue;

use crate::ecb_id::EcbId;
use crate::iptr::{IPtr, TypedIPtr};
use crate::{
    epic_control::{self, CombinatorContext, DependencyRef, DownstreamTaskDependency},
    ArgumentIdx, ObjectId, ObjectVersion,
};

pub type HostId = String;
pub type HostIdx = u64;

#[derive(Serialize, Deserialize, Copy, Clone, Debug)]
pub enum DependencyRefDef {
    EcbRef(EcbId),
}

impl From<DependencyRefDef> for DependencyRef {
    fn from(value: DependencyRefDef) -> Self {
        let DependencyRefDef::EcbRef(ecb_id) = value;
        Self::EcbRef(ecb_id)
    }
}

#[derive(Serialize, Deserialize, Copy, Clone, Debug)]
pub enum DownstreamTaskDependencyDef {
    None,
    DataDependency(DependencyRefDef, ArgumentIdx),
    ControlDependency(DependencyRefDef),
    ParentDependency(DependencyRefDef),
}

impl From<DownstreamTaskDependency> for DownstreamTaskDependencyDef {
    fn from(value: DownstreamTaskDependency) -> Self {
        match value {
            DownstreamTaskDependency::None => DownstreamTaskDependencyDef::None,
            DownstreamTaskDependency::DataDependency(dep_ref, arg_idx) => {
                let ecb_id = dep_ref.get_inner_ecb_id().unwrap();
                DownstreamTaskDependencyDef::DataDependency(
                    DependencyRefDef::EcbRef(ecb_id),
                    arg_idx,
                )
            }
            DownstreamTaskDependency::ControlDependency(dep_ref) => {
                let ecb_id = dep_ref.get_inner_ecb_id().unwrap();
                DownstreamTaskDependencyDef::ControlDependency(DependencyRefDef::EcbRef(ecb_id))
            }
            DownstreamTaskDependency::ParentDependency(dep_ref) => {
                let ecb_id = dep_ref.get_inner_ecb_id().unwrap();
                DownstreamTaskDependencyDef::ParentDependency(DependencyRefDef::EcbRef(ecb_id))
            }
        }
    }
}

impl Into<DownstreamTaskDependency> for DownstreamTaskDependencyDef {
    fn into(self) -> DownstreamTaskDependency {
        match self {
            DownstreamTaskDependencyDef::None => DownstreamTaskDependency::None,
            DownstreamTaskDependencyDef::DataDependency(dep_ref, arg_idx) => {
                DownstreamTaskDependency::DataDependency(dep_ref.into(), arg_idx)
            }
            DownstreamTaskDependencyDef::ControlDependency(dep_ref) => {
                DownstreamTaskDependency::ControlDependency(dep_ref.into())
            }
            DownstreamTaskDependencyDef::ParentDependency(dep_ref) => {
                DownstreamTaskDependency::ParentDependency(dep_ref.into())
            }
        }
    }
}

// TODO macros to specify type conversions for Args/Results

#[derive(Clone, Debug)]
pub enum NumberType {
    NumberI32(i32),
    NumberU8(u8),
    NumberU64(u64),
    NumberUS(usize),
    NumberU128(u128),
    NumberF64(f64),
    // TODO other types
}

impl TryInto<i32> for &NumberType {
    type Error = &'static str;

    fn try_into(self) -> Result<i32, Self::Error> {
        match self {
            NumberType::NumberU8(n) => Ok(*n as i32),
            NumberType::NumberU64(n) => {
                eprintln!("can't convert u64 {} to i32", n);
                Err("can't to convert u64 to i32")
            }
            NumberType::NumberI32(n) => Ok(*n),
            NumberType::NumberUS(_n) => Err("cannot convert usize to i32"),
            NumberType::NumberU128(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert u128 {} to u64: {}", n, e);
                    Err("failed to convert u128 to u64")
                }
            },
            NumberType::NumberF64(_n) => Err("unsupported conversion from floating-point to u64"),
        }
    }
}

impl TryInto<Option<i32>> for &NumberType {
    type Error = &'static str;

    fn try_into(self) -> Result<Option<i32>, Self::Error> {
        match self.try_into() {
            Ok(r) => Ok(Some(r)),
            Err(_) => {
                eprintln!(
                    "can't convert incompatible NumberType {:?} to Option<i32>",
                    self
                );
                Err("can't convert incompatible NumberType to Option<i32>")
            }
        }
    }
}

impl TryInto<Option<usize>> for &NumberType {
    type Error = &'static str;

    fn try_into(self) -> Result<Option<usize>, Self::Error> {
        match self.try_into() {
            Ok(r) => Ok(Some(r)),
            Err(_) => {
                eprintln!(
                    "can't convert incompatible NumberType {:?} to Option<i32>",
                    self
                );
                Err("can't convert incompatible NumberType to Option<i32>")
            }
        }
    }
}

impl TryInto<Option<ObjectId>> for &NumberType {
    type Error = &'static str;

    fn try_into(self) -> Result<Option<ObjectId>, Self::Error> {
        match self.try_into() {
            Ok(r) => Ok(Some(r)),
            Err(_) => {
                eprintln!(
                    "can't convert incompatible NumberType {:?} to Option<ObjectId>",
                    self
                );
                Err("can't convert incompatible NumberType to Option<ObjectId>")
            }
        }
    }
}

impl TryInto<usize> for &NumberType {
    type Error = &'static str;

    fn try_into(self) -> Result<usize, Self::Error> {
        match self {
            NumberType::NumberU8(n) => Ok(*n as usize),
            NumberType::NumberU64(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert u64 {} to usize: {}", n, e);
                    Err("failed to convert u64 to usize")
                }
            },
            NumberType::NumberI32(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert i32 {} to usize: {}", n, e);
                    Err("failed to convert i32 to usize")
                }
            },
            NumberType::NumberUS(n) => Ok(*n),
            NumberType::NumberU128(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert u128 {} to usize: {}", n, e);
                    Err("failed to convert u128 to usize")
                }
            },
            NumberType::NumberF64(_n) => Err("unsupported conversion from floating-point to usize"),
        }
    }
}

impl TryInto<u8> for &NumberType {
    type Error = &'static str;

    fn try_into(self) -> Result<u8, Self::Error> {
        match self {
            NumberType::NumberU8(n) => Ok(*n),
            NumberType::NumberU64(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert u64 {} to u8: {}", n, e);
                    Err("failed to convert u64 to u8")
                }
            },
            NumberType::NumberI32(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert i32 {} to u8: {}", n, e);
                    Err("failed to convert i32 to u8")
                }
            },
            NumberType::NumberU128(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert u128 {} to u8: {}", n, e);
                    Err("failed to convert u128 to u8")
                }
            },
            NumberType::NumberF64(_n) => Err("unsupported conversion from floating-point to u8"),
            NumberType::NumberUS(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert usize {} to u8: {}", n, e);
                    Err("failed to convert usize to u8")
                }
            },
        }
    }
}

impl TryInto<u64> for &NumberType {
    type Error = &'static str;

    fn try_into(self) -> Result<u64, Self::Error> {
        match self {
            NumberType::NumberU8(n) => Ok(*n as u64),
            NumberType::NumberU64(n) => Ok(*n),
            NumberType::NumberI32(n) => Ok(*n as u64),
            NumberType::NumberU128(n) => match (*n).try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("failed to convert u128 {} to u64: {}", n, e);
                    Err("failed to convert u128 to u64")
                }
            },
            NumberType::NumberF64(_n) => Err("unsupported conversion from floating-point to u64"),
            NumberType::NumberUS(n) => Ok(*n as u64),
        }
    }
}

impl TryInto<ObjectId> for &NumberType {
    type Error = &'static str;

    fn try_into(self) -> Result<ObjectId, Self::Error> {
        match self {
            NumberType::NumberU8(n) => Ok((*n).into()),
            NumberType::NumberU64(n) => Ok((*n).into()),
            NumberType::NumberI32(n) => Ok((*n as u64).into()),
            NumberType::NumberU128(n) => Ok(*n),
            NumberType::NumberF64(_n) => {
                Err("unsupported conversion from floating-point to ObjectId")
            }
            NumberType::NumberUS(n) => Ok(*n as u128),
        }
    }
}

impl From<&NumberType> for SerdeJsonValue {
    fn from(value: &NumberType) -> Self {
        match value {
            NumberType::NumberF64(f) => serde_json::to_value(*f).unwrap(),
            NumberType::NumberI32(n) => serde_json::to_value(*n).unwrap(),
            NumberType::NumberU8(n) => serde_json::to_value(*n).unwrap(),
            NumberType::NumberU64(n) => serde_json::to_value(*n).unwrap(),
            NumberType::NumberU128(n) => serde_json::to_value(n.to_string()).unwrap(),
            NumberType::NumberUS(n) => serde_json::to_value(*n).unwrap(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ScalarValue {
    Nil,
    Bool(bool),
    Number(NumberType),
    Str(String),
    Array(Vec<ScalarValue>),
    // TODO other types
}

impl ScalarValue {
    pub fn is_nil(&self) -> bool {
        match self {
            Self::Nil => true,
            _ => false,
        }
    }
}

impl From<&SerdeJsonValue> for ScalarValue {
    fn from(value: &SerdeJsonValue) -> Self {
        match value {
            SerdeJsonValue::Null => ScalarValue::Nil,
            SerdeJsonValue::Bool(b) => ScalarValue::Bool(*b),
            SerdeJsonValue::Number(n) => {
                if n.is_f64() {
                    let extracted_number = n.as_f64().expect("failed to parse f64");
                    return Self::Number(NumberType::NumberF64(extracted_number));
                }

                if n.is_i64() {
                    let extracted_number = n
                        .as_i64()
                        .expect("failed to parse i64")
                        .try_into()
                        .expect("could not fit i64 into i32");
                    return Self::Number(NumberType::NumberI32(extracted_number));
                }

                let extracted_number = n.as_u64().expect("failed to parse u64");
                Self::Number(NumberType::NumberU64(extracted_number))
            }
            SerdeJsonValue::String(s) => Self::Str(s.clone()),
            SerdeJsonValue::Array(ref vs) => Self::Array(vs.iter().map(|v| v.into()).collect()),
            _ => panic!("unexpected serde value {:?}", value),
        }
    }
}

impl From<&ScalarValue> for SerdeJsonValue {
    fn from(value: &ScalarValue) -> Self {
        match value {
            ScalarValue::Nil => SerdeJsonValue::Null,
            ScalarValue::Bool(b) => SerdeJsonValue::Bool(*b),
            ScalarValue::Str(s) => serde_json::to_value(s.clone()).unwrap(),
            ScalarValue::Number(n) => SerdeJsonValue::from(n),
            ScalarValue::Array(vs) => {
                SerdeJsonValue::from(vs.iter().map(|v| v.into()).collect::<Vec<SerdeJsonValue>>())
            }
        }
    }
}

impl<V> From<Option<V>> for ScalarValue
where
    V: Into<ScalarValue>,
{
    fn from(value: Option<V>) -> Self {
        match value {
            None => Self::Nil,
            Some(v) => match v.try_into() {
                Ok(v) => v,
                Err(_e) => {
                    eprintln!("failed to convert value in option type");
                    Self::Nil
                }
            },
        }
    }
}

/*
impl From<Option<i32>> for ScalarValue {
    fn from(value: Option<i32>) -> Self {
        match value {
            None => Self::Nil,
            Some(n) => n.try_into().unwrap(),
        }
    }
}
*/

impl From<bool> for ScalarValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i32> for ScalarValue {
    fn from(value: i32) -> Self {
        Self::Number(NumberType::NumberI32(value))
    }
}

impl From<usize> for ScalarValue {
    fn from(value: usize) -> Self {
        Self::Number(NumberType::NumberUS(value))
    }
}

impl From<u64> for ScalarValue {
    fn from(value: u64) -> Self {
        Self::Number(NumberType::NumberU64(value))
    }
}

impl From<u128> for ScalarValue {
    fn from(value: u128) -> Self {
        Self::Number(NumberType::NumberU128(value))
    }
}

impl From<String> for ScalarValue {
    fn from(value: String) -> Self {
        Self::Str(value.clone())
    }
}

impl TryInto<i32> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<i32, Self::Error> {
        match self {
            ScalarValue::Str(_s) => Err("value is a string, but a conversion to i32 was requested"),
            ScalarValue::Bool(_b) => {
                Err("value is a boolean, but a conversion to i32 was requested")
            }
            ScalarValue::Number(n) => match n.try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("Could not get number of value: {}", e);
                    Err("could not get number of value")
                }
            },
            ScalarValue::Nil => Ok(i32::default()),
            ScalarValue::Array(_) => {
                Err("value is an array, but a conversion to i32 was requested")
            }
        }
    }
}

impl TryInto<usize> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<usize, Self::Error> {
        match self {
            ScalarValue::Str(_s) => {
                Err("value is a string, but a conversion to usize was requested")
            }
            ScalarValue::Bool(_b) => {
                Err("value is a boolean, but a conversion to usize was requested")
            }
            ScalarValue::Number(n) => match n.try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("Could not get number of value: {}", e);
                    Err("could not get number of value")
                }
            },
            ScalarValue::Nil => Ok(usize::default()),
            ScalarValue::Array(_) => {
                Err("value is an array, but a conversion to usize was requested")
            }
        }
    }
}

impl TryInto<u8> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<u8, Self::Error> {
        match self {
            ScalarValue::Str(_s) => Err("value is a string, but a conversion to u8 was requested"),
            ScalarValue::Bool(_b) => {
                Err("value is a boolean, but a conversion to u8 was requested")
            }
            ScalarValue::Number(n) => match n.try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("Could not get number of value: {}", e);
                    Err("could not get number of value")
                }
            },
            ScalarValue::Nil => Ok(u8::default()),
            ScalarValue::Array(_) => Err("value is an array, but a conversion to u8 was requested"),
        }
    }
}

impl TryInto<u64> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<u64, Self::Error> {
        match self {
            ScalarValue::Str(_s) => Err("value is a string, but a conversion to u64 was requested"),
            ScalarValue::Bool(_b) => {
                Err("value is a boolean, but a conversion to u64 was requested")
            }
            ScalarValue::Number(n) => match n.try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("Could not get number of value: {}", e);
                    Err("could not get number of value")
                }
            },
            ScalarValue::Nil => Ok(u64::default()),
            ScalarValue::Array(_) => {
                Err("value is an array, but a conversion to u64 was requested")
            }
        }
    }
}

impl TryInto<ObjectId> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<ObjectId, Self::Error> {
        match self {
            ScalarValue::Str(s) => match s.parse() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("Could not parse string as u128: {}", e);
                    Err("could not parse string as u128")
                }
            },
            ScalarValue::Bool(_b) => {
                Err("value is a boolean, but a conversion to u128 was requested")
            }
            ScalarValue::Number(n) => match n.try_into() {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("Could not get number of value: {}", e);
                    Err("could not get number of value")
                }
            },
            ScalarValue::Nil => Ok(ObjectId::default()),
            ScalarValue::Array(_) => {
                Err("value is an array, but a conversion to u128 was requested")
            }
        }
    }
}

impl TryInto<String> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<String, Self::Error> {
        match self {
            ScalarValue::Str(s) => Ok(s.to_string()),
            ScalarValue::Bool(b) => Ok(b.to_string()),
            ScalarValue::Number(n) => {
                eprintln!("Could not get string of non-string value: {:?}", n);
                Err("could not get string of non-string value")
            }
            ScalarValue::Nil => Ok(String::default()),
            ScalarValue::Array(_) => Err("Could not get string from array"),
        }
    }
}

impl TryInto<bool> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<bool, Self::Error> {
        match self {
            ScalarValue::Bool(b) => Ok(*b),
            ScalarValue::Str(_s) => {
                Err("value is a string, but a conversion to boolean was requested")
            }
            ScalarValue::Number(_n) => {
                Err("value is a number, but a conversion to boolean was requested")
            }
            ScalarValue::Array(_) => {
                Err("value is an array, but a conversion to boolean was requested")
            }
            ScalarValue::Nil => Ok(bool::default()),
        }
    }
}

impl TryInto<Option<i32>> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<Option<i32>, Self::Error> {
        match self {
            ScalarValue::Nil => Ok(None),
            ScalarValue::Number(n) => n.try_into(),
            ScalarValue::Str(_) => Err("cannot convert string to Option<i32>"),
            ScalarValue::Bool(_) => Err("cannot convert boolean to Option<i32>"),
            ScalarValue::Array(_) => Err("cannot convert array to Option<i32>"),
        }
    }
}

impl TryInto<Option<usize>> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<Option<usize>, Self::Error> {
        match self {
            ScalarValue::Nil => Ok(None),
            ScalarValue::Number(n) => n.try_into(),
            ScalarValue::Str(_) => Err("cannot convert string to Option<i32>"),
            ScalarValue::Bool(_) => Err("cannot convert boolean to Option<i32>"),
            ScalarValue::Array(_) => Err("cannot convert array to Option<i32>"),
        }
    }
}

impl TryInto<Option<String>> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<Option<String>, Self::Error> {
        match self {
            ScalarValue::Nil => Ok(None),
            ScalarValue::Number(_) => Err("cannot convert number to Option<String>"),
            ScalarValue::Str(s) => Ok(Some(s.clone())),
            ScalarValue::Bool(_) => Err("cannot convert boolean to Option<String>"),
            ScalarValue::Array(_) => Err("cannot convert array to Option<String>"),
        }
    }
}

impl TryInto<Option<ObjectId>> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<Option<ObjectId>, Self::Error> {
        match self {
            ScalarValue::Nil => Ok(None),
            ScalarValue::Number(n) => n.try_into(),
            ScalarValue::Str(_) => Err("cannot convert string to Option<ObjectId>"),
            ScalarValue::Bool(_) => Err("cannot convert boolean to Option<ObjectId>"),
            ScalarValue::Array(_) => Err("cannot convert array to Option<ObjectId>"),
        }
    }
}

impl TryInto<Vec<String>> for &ScalarValue {
    type Error = &'static str;

    fn try_into(self) -> Result<Vec<String>, Self::Error> {
        match self {
            ScalarValue::Str(_) => Err("could not parse string as vec of values"),
            ScalarValue::Bool(_) => {
                Err("value is a boolean, but a conversion to an array was requested")
            }
            ScalarValue::Number(_) => {
                Err("value is a number, but a conversion to an array was requested")
            }
            ScalarValue::Nil => Ok(Vec::default()),
            ScalarValue::Array(vs) => {
                let mut res = Vec::with_capacity(vs.len());
                for v in vs {
                    match v.try_into() {
                        Err(_) => return Err("failed to convert value in array to desired value"),
                        Ok(cv) => res.push(cv),
                    }
                }

                Ok(res)
            }
        }
    }
}

// FIXME below doesn't work
/*
impl<V> TryInto<Vec<V>> for &'a ScalarValue
where
    V: TryFrom<&'a ScalarValue>,
{
    type Error = &'static str;

    fn try_into(self) -> Result<Vec<V>, Self::Error> {
        match self {
            ScalarValue::Str(_) => Err("could not parse string as vec of values"),
            ScalarValue::Bool(_b) => {
                Err("value is a boolean, but a conversion to an array was requested")
            }
            ScalarValue::Number(n) => {
                Err("value is a number, but a conversion to an array was requested")
            }
            ScalarValue::Nil => Ok(Vec::default()),
            ScalarValue::Array(vs) => {
                let mut res = Vec::with_capacity(vs.len());
                for v in vs {
                    match v.try_into() {
                        Err(_) => return Err("failed to convert value in array to desired value"),
                        Ok(cv) => res.push(cv),
                    }
                }

                Ok(res)
            }
        }
    }
}
*/

// NOTE ideally the below would be an untagged enum, but serde tragically fails to parse refs
// correctly if we do mark this as untagged, so this will do for now.
#[derive(Clone, Debug)]
pub enum NandoArgument {
    Ref(IPtr),
    MRef(IPtrList),
    Value(ScalarValue),
    UnresolvedArgument(usize),
    TaskedUnresolvedArgument((EcbId, usize)),
}

impl NandoArgument {
    pub fn get_nil() -> Self {
        Self::Value(ScalarValue::Nil)
    }

    pub fn is_nil(&self) -> bool {
        match self {
            Self::Value(ref v) => v.is_nil(),
            _ => false,
        }
    }

    pub fn is_unresolved(&self) -> bool {
        match self {
            Self::UnresolvedArgument(_) | Self::TaskedUnresolvedArgument(_) => true,
            _ => false,
        }
    }

    pub fn get_string(&self) -> Option<String> {
        match self {
            Self::Value(ScalarValue::Str(ref string)) => Some(string.clone()),
            _ => None,
        }
    }

    pub fn from_object_id(object_id: ObjectId) -> Self {
        Self::Ref(IPtr::new(object_id, 0, 0))
    }

    pub fn get_object_id(&self) -> Option<ObjectId> {
        match self {
            Self::Ref(ref iptr) => Some(iptr.get_object_id()),
            _ => None,
        }
    }

    pub fn get_object_ids(&self) -> Vec<ObjectId> {
        match self {
            Self::Ref(ref v) => vec![v.get_object_id()],
            Self::MRef(v) => v.l.iter().map(|iptr| iptr.get_object_id()).collect(),
            _ => vec![],
        }
    }

    pub fn get_unresolved_arg_idx(&self) -> Option<usize> {
        match self {
            Self::UnresolvedArgument(idx) | Self::TaskedUnresolvedArgument((_, idx)) => Some(*idx),
            _ => None,
        }
    }

    pub fn get_unresolved_arg_task(&self) -> Option<EcbId> {
        match self {
            Self::TaskedUnresolvedArgument((ecb_id, _)) => Some(*ecb_id),
            _ => None,
        }
    }

    pub fn get_u64_value(&self) -> Option<u64> {
        match self {
            Self::Value(v) => match v.try_into() {
                Err(_) => None,
                Ok(v) => Some(v),
            },
            _ => None,
        }
    }
}

impl From<&NandoArgumentSerializable> for NandoArgument {
    fn from(value: &NandoArgumentSerializable) -> Self {
        match value {
            NandoArgumentSerializable::Ref(iptr) => Self::Ref(*iptr),
            NandoArgumentSerializable::MRef(iptrs) => Self::MRef(iptrs.clone()),
            NandoArgumentSerializable::Value(v) => Self::Value(ScalarValue::from(v)),
            NandoArgumentSerializable::UnresolvedArgument(v) => Self::UnresolvedArgument(*v),
            NandoArgumentSerializable::TaskedUnresolvedArgument((ecb_id, v)) => {
                Self::TaskedUnresolvedArgument((*ecb_id, *v))
            }
            NandoArgumentSerializable::Epic(_) => todo!("not sure if this should be exercised"),
        }
    }
}

impl<V> From<Option<V>> for NandoArgument
where
    ScalarValue: From<Option<V>>,
{
    fn from(value: Option<V>) -> Self {
        NandoArgument::Value(ScalarValue::from(value))
    }
}

impl From<bool> for NandoArgument {
    fn from(value: bool) -> Self {
        NandoArgument::Value(ScalarValue::from(value))
    }
}

impl From<i32> for NandoArgument {
    fn from(value: i32) -> Self {
        NandoArgument::Value(ScalarValue::from(value))
    }
}

impl From<usize> for NandoArgument {
    fn from(value: usize) -> Self {
        NandoArgument::Value(ScalarValue::from(value))
    }
}

impl From<u64> for NandoArgument {
    fn from(value: u64) -> Self {
        NandoArgument::Value(ScalarValue::from(value))
    }
}

impl From<u128> for NandoArgument {
    fn from(value: u128) -> Self {
        NandoArgument::Value(ScalarValue::from(value))
    }
}

impl From<String> for NandoArgument {
    fn from(value: String) -> Self {
        NandoArgument::Value(ScalarValue::from(value))
    }
}

impl From<&ScalarValue> for NandoArgument {
    fn from(value: &ScalarValue) -> Self {
        match value {
            ScalarValue::Nil => Self::Value(ScalarValue::Nil),
            ScalarValue::Bool(b) => Self::Value(ScalarValue::Bool(*b)),
            ScalarValue::Number(n) => Self::Value(ScalarValue::Number(n.clone())),
            ScalarValue::Str(s) => Self::Value(ScalarValue::Str(s.clone())),
            ScalarValue::Array(ref vs) => Self::Value(ScalarValue::Array(vs.clone())),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct IPtrList {
    pub(crate) l: Vec<IPtr>,
}

impl IPtrList {
    pub fn from_vec(v: Vec<IPtr>) -> Self {
        Self { l: v }
    }

    pub fn push(&mut self, v: IPtr) {
        self.l.push(v);
    }

    pub fn get_inner(&self) -> &Vec<IPtr> {
        &self.l
    }
}

impl<'a, T> From<&'a Vec<T>> for IPtrList
where
    IPtr: From<&'a T>,
{
    fn from(value: &'a Vec<T>) -> Self {
        Self::from_vec(value.iter().map(|e| IPtr::from(e)).collect())
    }
}

impl<'a, T> Into<Vec<T>> for &'a IPtrList
where
    T: From<&'a IPtr>,
{
    fn into(self) -> Vec<T> {
        self.get_inner().iter().map(|e| T::from(e)).collect()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum NandoArgumentSerializable {
    Ref(IPtr),
    MRef(IPtrList),
    Value(SerdeJsonValue),
    UnresolvedArgument(usize),
    TaskedUnresolvedArgument((EcbId, usize)),
    Epic(EcbId),
}

impl From<&Vec<IPtr>> for IPtrList {
    fn from(value: &Vec<IPtr>) -> Self {
        IPtrList {
            // l: value.iter().map(|i| i.into()).collect(),
            l: value.clone(),
        }
    }
}

impl From<IPtr> for NandoArgument {
    fn from(value: IPtr) -> Self {
        Self::Ref(value)
    }
}

impl<T> From<TypedIPtr<T>> for NandoArgument {
    fn from(value: TypedIPtr<T>) -> Self {
        Self::Ref(value.get_inner())
    }
}

impl From<&IPtr> for NandoArgument {
    fn from(value: &IPtr) -> Self {
        Self::Ref(*value)
    }
}

impl From<Vec<IPtr>> for NandoArgument {
    fn from(value: Vec<IPtr>) -> Self {
        Self::MRef(IPtrList::from_vec(value))
    }
}

impl From<Vec<String>> for NandoArgument {
    fn from(values: Vec<String>) -> Self {
        Self::Value(ScalarValue::Array(
            values.into_iter().map(|v| v.into()).collect(),
        ))
    }
}

impl From<&Vec<String>> for NandoArgument {
    fn from(values: &Vec<String>) -> Self {
        Self::Value(ScalarValue::Array(
            values.iter().map(|v| v.clone().into()).collect(),
        ))
    }
}

impl<T> From<&TypedIPtr<T>> for NandoArgument {
    fn from(value: &TypedIPtr<T>) -> Self {
        Self::Ref(value.get_inner())
    }
}

impl From<&NandoArgument> for NandoArgumentSerializable {
    fn from(value: &NandoArgument) -> Self {
        match value {
            NandoArgument::Ref(iptr) => Self::Ref(*iptr),
            NandoArgument::MRef(v) => Self::MRef(v.clone()),
            NandoArgument::Value(v) => Self::Value(v.into()),
            NandoArgument::UnresolvedArgument(idx) => Self::UnresolvedArgument(*idx),
            NandoArgument::TaskedUnresolvedArgument((ecb_id, idx)) => {
                Self::TaskedUnresolvedArgument((*ecb_id, *idx))
            }
        }
    }
}

pub type NandoResult = NandoArgument;
pub type NandoResultSerializable = NandoArgumentSerializable;

impl<V> From<V> for NandoResultSerializable
where
    V: Into<SerdeJsonValue>,
{
    fn from(value: V) -> Self {
        Self::Value(value.into())
    }
}

impl NandoArgumentSerializable {
    pub fn get_nil() -> Self {
        Self::Value(SerdeJsonValue::Null)
    }

    pub fn get_object_id(&self) -> Option<ObjectId> {
        match self {
            Self::Ref(ref iptr) => Some(iptr.get_object_id()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NandoActivationIntent {
    pub host_idx: Option<HostIdx>,
    pub name: String,
    pub args: Vec<NandoArgument>,
}

impl NandoActivationIntent {
    pub fn new_for(intent_name: String) -> Self {
        Self {
            host_idx: None,
            name: intent_name,
            args: vec![],
        }
    }

    pub fn is_built_in(&self) -> bool {
        !self.name.contains("::")
    }

    pub fn new_executor_stats_intent() -> Self {
        Self {
            host_idx: None,
            name: "log_executor_stats".to_string(),
            args: vec![],
        }
    }

    pub fn is_executor_stats_intent(&self) -> bool {
        self.name == "log_executor_stats"
    }

    pub fn get_object_references(&self) -> Vec<ObjectId> {
        // FIXME perf
        let mut res = HashSet::new();
        for arg in &self.args {
            match arg {
                NandoArgument::Ref(iptr) => {
                    res.insert(iptr.object_id);
                }
                NandoArgument::MRef(iptr_list) => {
                    res.extend(iptr_list.get_inner().iter().map(|i| i.get_object_id()));
                }
                _ => continue,
            };
        }

        res.into_iter().collect()
    }

    pub fn get_object_arg_indices(&self) -> Vec<usize> {
        self.args
            .iter()
            .enumerate()
            .filter(|(_, arg)| match arg {
                NandoArgument::Ref(_) => true,
                NandoArgument::MRef(_) => true,
                _ => false,
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn resolve_intent_arg(
        &mut self,
        idx: usize,
        resolved_arg: NandoArgument,
        resolving_task: Option<EcbId>,
    ) {
        let current_value = &self.args[idx];

        match current_value {
            NandoArgument::UnresolvedArgument(_) => self.args[idx] = resolved_arg,
            NandoArgument::TaskedUnresolvedArgument(_) => self.args[idx] = resolved_arg,
            NandoArgument::MRef(_) => {
                let NandoArgument::MRef(ref mut existing_list) = self.args[idx] else {
                    unreachable!();
                };
                match resolved_arg {
                    NandoArgument::Ref(i) => existing_list.push(i),
                    NandoArgument::MRef(is) => existing_list.l.extend(&is.l),
                    _ => unreachable!(),
                }

                let resolving_task = resolving_task.unwrap();
                self.args.retain(|e| {
                    let NandoArgument::TaskedUnresolvedArgument((task_id, _)) = e else {
                        return true;
                    };

                    resolving_task != *task_id
                });
            }
            _ => unreachable!(
                "Cannot resolve arg {idx}: {:?} / {:?}",
                current_value, resolved_arg
            ),
        }
    }

    pub fn has_unresolved_args(&self) -> bool {
        self.args.iter().any(|e| match e {
            NandoArgument::UnresolvedArgument(_) | NandoArgument::TaskedUnresolvedArgument(_) => {
                true
            }
            _ => false,
        })
    }

    pub fn prepend_object_argument(&mut self, arg: IPtr) {
        self.args.insert(0, NandoArgument::Ref(arg));
    }

    pub fn is_spawn_cache_intent(&self) -> bool {
        self.name == "spawn_cache"
    }

    pub fn is_invalidation_intent(&self) -> bool {
        self.name == "invalidate"
    }

    pub fn is_set_caching_permissible(&self) -> bool {
        self.name == "set_caching_permissible"
    }

    pub fn is_invalidation_spawn_intent(&self) -> bool {
        self.name == "spawn_cache_invalidations"
    }

    pub fn is_update_caches_intent(&self) -> bool {
        self.name == "update_caches"
    }

    pub fn is_update_caches_internal_intent(&self) -> bool {
        self.name == "update_caches_internal"
    }

    pub fn is_whomstone_intent(&self) -> bool {
        self.name.contains("whomstone")
    }

    pub fn is_whomstone_and_move_intent(&self) -> bool {
        self.name.ends_with("whomstone_and_move")
    }

    pub fn is_cache_related_intent(&self) -> bool {
        self.name.starts_with("cache_")
    }

    pub fn set_host_idx(&mut self, host_idx: HostIdx) {
        self.host_idx = Some(host_idx);
    }
}

impl From<NandoActivationIntentSerializable> for NandoActivationIntent {
    fn from(value: NandoActivationIntentSerializable) -> Self {
        Self {
            host_idx: value.host_idx,
            name: value.name,
            args: value.args.iter().map(|a| a.into()).collect(),
        }
    }
}

impl From<&NandoActivationIntent> for NandoActivationIntentSerializable {
    fn from(value: &NandoActivationIntent) -> Self {
        Self {
            host_idx: value.host_idx,
            name: value.name.clone(),
            args: value.args.iter().map(|a| a.into()).collect(),
            with_plan: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NandoActivationIntentSerializable {
    pub host_idx: Option<HostIdx>,
    pub name: String,
    pub args: Vec<NandoArgumentSerializable>,
    // Optionally include the name of a physical plan to follow.
    pub with_plan: Option<String>,
}

impl NandoActivationIntentSerializable {
    pub fn get_object_references(&self) -> Vec<ObjectId> {
        // FIXME perf
        let mut res = HashSet::new();
        for arg in &self.args {
            match arg {
                &NandoArgumentSerializable::Ref(iptr) => res.insert(iptr.object_id),
                _ => continue,
            };
        }

        res.into_iter().collect()
    }

    pub fn prepend_object_argument(&mut self, arg: IPtr) {
        self.args.insert(0, NandoArgumentSerializable::Ref(arg));
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SchedulerIntent {
    pub intent: NandoActivationIntentSerializable,
    pub mutable_argument_indices: Vec<usize>,
}

impl SchedulerIntent {
    pub fn get_object_references(&self) -> Vec<ObjectId> {
        self.intent.get_object_references()
    }

    pub fn get_mut_object_references(&self) -> Vec<ObjectId> {
        self.intent
            .args
            .iter()
            .enumerate()
            .filter(|(idx, _)| self.mutable_argument_indices.contains(idx))
            .map(|(_, r)| r.get_object_id().unwrap())
            .collect()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SpawnedTaskSerializable {
    pub id: EcbId,
    pub intent: NandoActivationIntentSerializable,
    pub parent_task: DownstreamTaskDependencyDef,
    pub downstream_dependents: Vec<DownstreamTaskDependencyDef>,
    pub upstream_control_dependencies: HashSet<EcbId>,

    #[cfg(feature = "object-caching")]
    pub mask_invalidations: bool,

    pub is_fold: bool,
}

impl From<&epic_control::SpawnedTask> for SpawnedTaskSerializable {
    fn from(value: &epic_control::SpawnedTask) -> Self {
        Self {
            id: value.id,
            intent: (&value.intent).into(),
            parent_task: value.parent_task.clone().into(),
            downstream_dependents: value
                .downstream_dependents
                .iter()
                .map(|dd| dd.clone().into())
                .collect(),
            upstream_control_dependencies: value.upstream_control_dependencies.clone(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: value.mask_invalidations,

            is_fold: value.is_fold(),
        }
    }
}

impl From<&SpawnedTaskSerializable> for epic_control::SpawnedTask {
    fn from(value: &SpawnedTaskSerializable) -> Self {
        Self {
            id: value.id,
            intent: value.intent.clone().into(),
            parent_task: value.parent_task.into(),
            downstream_dependents: value
                .downstream_dependents
                .iter()
                .map(|dd| (*dd).into())
                .collect(),
            upstream_control_dependencies: value.upstream_control_dependencies.clone(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: value.mask_invalidations,

            // FIXME @physical
            planning_context: epic_control::PlanningContext::default(),

            combinator_context: match value.is_fold {
                false => CombinatorContext::NoCombinator,
                // FIXME
                true => CombinatorContext::Fold {
                    can_subdivide: false,
                },
            },
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TaskCompletionSerializable {
    pub id: EcbId,
    pub tasks_to_notify: Vec<(
        DownstreamTaskDependencyDef,
        NandoActivationExecutionStatus,
        Option<NandoResultSerializable>,
    )>,
    pub notifying_host: HostIdx,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum NandoActivationExecutionStatus {
    Success,
    EpicSpawned(EcbId),
    Error(String),
}

impl From<&epic_control::TaskStatus> for NandoActivationExecutionStatus {
    fn from(value: &epic_control::TaskStatus) -> Self {
        match value {
            epic_control::TaskStatus::Success => Self::Success,
            epic_control::TaskStatus::Failure => Self::Error("n/a".to_string()),
            _ => panic!(),
        }
    }
}

impl Into<epic_control::TaskStatus> for NandoActivationExecutionStatus {
    fn into(self) -> epic_control::TaskStatus {
        match self {
            Self::Success => epic_control::TaskStatus::Success,
            Self::Error(_) => epic_control::TaskStatus::Failure,
            _ => panic!(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum NandoActivationStatus {
    Executed(NandoActivationExecutionStatus),
    RecomputedActivationSite(HostId),
}

#[derive(Serialize, Deserialize)]
pub struct NandoActivationResolution {
    pub status: NandoActivationStatus,
    pub output: Vec<NandoResultSerializable>,
    pub cacheable_objects: Option<Vec<(ObjectId, ObjectVersion)>>,
}

#[derive(Serialize, Deserialize)]
pub struct ScheduleResponse {
    pub target_host: HostId,
    pub had_to_consolidate: bool,
}
