use std::{error::Error, fmt, io, mem::MaybeUninit};

use dokan_sys::{
	DokanGetRuntimeIdentity, DOKAN_DRIVER_CAPABILITY_RUNTIME_IDENTITY, DOKAN_PROFILE_HASH,
	DOKAN_PROTOCOL_ABI, DOKAN_RUNTIME_IDENTITY, DOKAN_RUNTIME_IDENTITY_SCHEMA_VERSION,
};
use winapi::shared::{guiddef::GUID, minwindef::FALSE};

/// The immutable identifier of a Dokany driver family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DistributionFamilyId {
	pub data1: u32,
	pub data2: u16,
	pub data3: u16,
	pub data4: [u8; 8],
}

impl From<GUID> for DistributionFamilyId {
	fn from(value: GUID) -> Self {
		Self {
			data1: value.Data1,
			data2: value.Data2,
			data3: value.Data3,
			data4: value.Data4,
		}
	}
}

/// Identity and capabilities reported by the opened Dokany kernel driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeIdentity {
	pub schema_version: u32,
	pub protocol_abi: u32,
	pub driver_version: u32,
	pub capabilities: u64,
	pub family_id: DistributionFamilyId,
	pub profile_hash: [u8; 32],
}

impl RuntimeIdentity {
	fn from_raw(raw: DOKAN_RUNTIME_IDENTITY) -> Result<Self, RuntimeIdentityError> {
		let expected_size = std::mem::size_of::<DOKAN_RUNTIME_IDENTITY>() as u32;
		if raw.Size != expected_size {
			return Err(RuntimeIdentityError::InvalidSize {
				expected: expected_size,
				actual: raw.Size,
			});
		}
		if raw.SchemaVersion != DOKAN_RUNTIME_IDENTITY_SCHEMA_VERSION {
			return Err(RuntimeIdentityError::UnsupportedSchema {
				expected: DOKAN_RUNTIME_IDENTITY_SCHEMA_VERSION,
				actual: raw.SchemaVersion,
			});
		}
		if raw.Capabilities & DOKAN_DRIVER_CAPABILITY_RUNTIME_IDENTITY == 0 {
			return Err(RuntimeIdentityError::MissingIdentityCapability);
		}

		Ok(Self {
			schema_version: raw.SchemaVersion,
			protocol_abi: raw.ProtocolAbi,
			driver_version: raw.DriverVersion,
			capabilities: raw.Capabilities,
			family_id: raw.FamilyGuid.into(),
			profile_hash: raw.ProfileHash,
		})
	}

	/// Returns whether this driver was compiled from exactly the same distribution profile.
	pub fn matches_compiled_distribution(&self) -> bool {
		self.protocol_abi == DOKAN_PROTOCOL_ABI && self.profile_hash == DOKAN_PROFILE_HASH
	}
}

/// Failure to query or validate the installed Dokany driver identity.
#[derive(Debug)]
pub enum RuntimeIdentityError {
	Query(io::Error),
	InvalidSize {
		expected: u32,
		actual: u32,
	},
	UnsupportedSchema {
		expected: u32,
		actual: u32,
	},
	MissingIdentityCapability,
	DistributionMismatch {
		expected_protocol_abi: u32,
		actual_protocol_abi: u32,
		expected_profile_hash: [u8; 32],
		actual_profile_hash: [u8; 32],
	},
}

impl fmt::Display for RuntimeIdentityError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Query(error) => write!(formatter, "failed to query Dokany runtime identity: {error}"),
			Self::InvalidSize { expected, actual } => write!(
				formatter,
				"invalid Dokany runtime identity size: expected {expected}, received {actual}"
			),
			Self::UnsupportedSchema { expected, actual } => write!(
				formatter,
				"unsupported Dokany runtime identity schema: expected {expected}, received {actual}"
			),
			Self::MissingIdentityCapability => {
				formatter.write_str("Dokany driver does not advertise runtime identity support")
			}
			Self::DistributionMismatch {
				expected_protocol_abi,
				actual_protocol_abi,
				..
			} => write!(
				formatter,
				"Dokany distribution mismatch: expected protocol ABI {expected_protocol_abi}, received {actual_protocol_abi}"
			),
		}
	}
}

impl Error for RuntimeIdentityError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Query(error) => Some(error),
			_ => None,
		}
	}
}

/// Queries the immutable identity of the installed driver addressed by this DLL family.
pub fn query_runtime_identity() -> Result<RuntimeIdentity, RuntimeIdentityError> {
	let mut raw = MaybeUninit::<DOKAN_RUNTIME_IDENTITY>::uninit();
	if unsafe { DokanGetRuntimeIdentity(raw.as_mut_ptr()) } == FALSE {
		return Err(RuntimeIdentityError::Query(io::Error::last_os_error()));
	}
	RuntimeIdentity::from_raw(unsafe { raw.assume_init() })
}

/// Queries the driver and rejects any build not produced from this crate's distribution profile.
pub fn verify_runtime_identity() -> Result<RuntimeIdentity, RuntimeIdentityError> {
	let identity = query_runtime_identity()?;
	if !identity.matches_compiled_distribution() {
		return Err(RuntimeIdentityError::DistributionMismatch {
			expected_protocol_abi: DOKAN_PROTOCOL_ABI,
			actual_protocol_abi: identity.protocol_abi,
			expected_profile_hash: DOKAN_PROFILE_HASH,
			actual_profile_hash: identity.profile_hash,
		});
	}
	Ok(identity)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn compatible_raw_identity() -> DOKAN_RUNTIME_IDENTITY {
		DOKAN_RUNTIME_IDENTITY {
			Size: std::mem::size_of::<DOKAN_RUNTIME_IDENTITY>() as u32,
			SchemaVersion: DOKAN_RUNTIME_IDENTITY_SCHEMA_VERSION,
			ProtocolAbi: DOKAN_PROTOCOL_ABI,
			DriverVersion: 231,
			Capabilities: DOKAN_DRIVER_CAPABILITY_RUNTIME_IDENTITY,
			FamilyGuid: GUID {
				Data1: 0x1682_1989,
				Data2: 0x576d,
				Data3: 0x48a9,
				Data4: [0xb9, 0xcd, 0xf6, 0x94, 0x16, 0xf4, 0x26, 0x38],
			},
			ProfileHash: DOKAN_PROFILE_HASH,
		}
	}

	#[test]
	fn accepts_the_compiled_distribution() {
		let identity = RuntimeIdentity::from_raw(compatible_raw_identity()).unwrap();
		assert!(identity.matches_compiled_distribution());
	}

	#[test]
	fn rejects_unknown_wire_schema() {
		let mut raw = compatible_raw_identity();
		raw.SchemaVersion += 1;
		assert!(matches!(
			RuntimeIdentity::from_raw(raw),
			Err(RuntimeIdentityError::UnsupportedSchema { .. })
		));
	}

	#[test]
	fn detects_profile_mismatch() {
		let mut raw = compatible_raw_identity();
		raw.ProfileHash[0] ^= 0xff;
		let identity = RuntimeIdentity::from_raw(raw).unwrap();
		assert!(!identity.matches_compiled_distribution());
	}
}
