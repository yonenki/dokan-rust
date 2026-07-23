use std::{
	error::Error,
	fmt::{self, Display, Formatter},
	marker::PhantomData,
	os::windows::io::{AsRawHandle, BorrowedHandle},
	ptr,
	time::Duration,
};

use bitflags::bitflags;
use dokan_sys::{
	DokanCloseHandle, DokanCreateFileSystem, DokanCreateFileSystemEx, DokanRequestUnmount,
	DokanWaitForFileSystemClosed, DOKAN_CANCELLED_ERROR, DOKAN_DRIVER_FEATURE_ERROR,
	DOKAN_DRIVER_INSTALL_ERROR, DOKAN_DRIVE_LETTER_ERROR, DOKAN_ERROR, DOKAN_HANDLE,
	DOKAN_MOUNT_ERROR, DOKAN_MOUNT_POINT_ERROR, DOKAN_OPERATIONS, DOKAN_OPTIONS,
	DOKAN_OPTION_ALLOW_IPC_BATCHING, DOKAN_OPTION_ALT_STREAM, DOKAN_OPTION_CASE_SENSITIVE,
	DOKAN_OPTION_CURRENT_SESSION, DOKAN_OPTION_DEBUG, DOKAN_OPTION_DISPATCH_DRIVER_LOGS,
	DOKAN_OPTION_ENABLE_UNMOUNT_NETWORK_DRIVE, DOKAN_OPTION_FILELOCK_USER_MODE,
	DOKAN_OPTION_MOUNT_MANAGER, DOKAN_OPTION_NETWORK, DOKAN_OPTION_REMOVABLE, DOKAN_OPTION_STDERR,
	DOKAN_OPTION_WRITE_PROTECT, DOKAN_START_ERROR, DOKAN_SUCCESS, DOKAN_VERSION_ERROR,
	VOLUME_SECURITY_DESCRIPTOR_MAX_SIZE,
};
use widestring::U16CStr;
use winapi::{shared::ntdef::SCHAR, um::winbase::INFINITE};

use crate::{file_system_handler::FileSystemHandler, operations, WRAPPER_VERSION};

bitflags! {
	/// Flags that control behavior of the mounted volume, as part of [`MountOptions`].
	#[derive(Debug, Clone, Eq, PartialEq)]
	pub struct MountFlags : u32 {
		/// Enable debug message output.
		const DEBUG = DOKAN_OPTION_DEBUG;

		/// Write debug messages to stderr.
		const STDERR = DOKAN_OPTION_STDERR;

		/// Enable support for alternative streams.
		///
		/// The driver will fail any attempts to access a path with a colon (`:`).
		const ALT_STREAM = DOKAN_OPTION_ALT_STREAM;

		/// Make the mounted volume write-protected (i.e. read-only).
		const WRITE_PROTECT = DOKAN_OPTION_WRITE_PROTECT;

		/// Mount as a network drive.
		///
		/// Dokan network provider must be installed for this to work.
		const NETWORK = DOKAN_OPTION_NETWORK;

		/// Mount as a removable device.
		const REMOVABLE = DOKAN_OPTION_REMOVABLE;

		/// Use Mount Manager to mount the volume.
		const MOUNT_MANAGER = DOKAN_OPTION_MOUNT_MANAGER;

		/// Mount the volume on current session only.
		const CURRENT_SESSION = DOKAN_OPTION_CURRENT_SESSION;

		/// Use [`FileSystemHandler::lock_file`] and [`FileSystemHandler::unlock_file`] to handle
		/// file locking.
		///
		/// Dokan will take care of file locking if this flags is not present.
		const FILELOCK_USER_MODE = DOKAN_OPTION_FILELOCK_USER_MODE;

		/// Case sensitive path.
		///
		///	By default all paths are case insensitive.
		///
		///	For case sensitive: `\dir\File` & `\diR\file` are different files,
		///	but for case insensitive they are the same.
		const CASE_SENSITIVE = DOKAN_OPTION_CASE_SENSITIVE;

		/// Allow unmounting network drives from Windows Explorer.
		const ENABLE_UNMOUNT_NETWORK_DRIVE = DOKAN_OPTION_ENABLE_UNMOUNT_NETWORK_DRIVE;

		/// Forward the kernel driver global and volume logs to the userland.
		const DISPATCH_DRIVER_LOGS = DOKAN_OPTION_DISPATCH_DRIVER_LOGS;

		/// Pull batches of events from the driver instead of a single one and execute them parallelly.
		/// This option should only be used on computers with low cpu count
		/// and userland filesystem taking time to process requests (like remote storage).
		const ALLOW_IPC_BATCHING = DOKAN_OPTION_ALLOW_IPC_BATCHING;
	}
}

/// Options for [`FileSystemMounter::new`].
pub struct MountOptions<'a> {
	/// Only use a single thread to process events. This is highly not recommended as can easily create a bottleneck.
	pub single_thread: bool,

	/// Controls behavior of the volume.
	pub flags: MountFlags,

	/// UNC Name for the Network Redirector.
	///
	/// See [Support for UNC Naming].
	///
	/// [Support for UNC Naming]: https://msdn.microsoft.com/en-us/library/windows/hardware/ff556761(v=vs.85).aspx
	pub unc_name: Option<&'a U16CStr>,

	/// Max timeout of each request before Dokan gives up to wait events to complete.
	/// Timeout request is a sign that the userland implementation is no longer able to properly manage requests in time.
	/// The driver will therefore unmount the device when a timeout trigger in order to keep the system stable.
	///
	/// This timeout can be temporarily extended for an operation with
	/// [`OperationInfo::reset_timeout`].
	///
	/// If zero, defaults to 15 seconds.
	///
	/// [`OperationInfo::reset_timeout`]: crate::OperationInfo::reset_timeout
	pub timeout: Duration,

	/// Allocation Unit Size of the volume. This will affect the file size.
	pub allocation_unit_size: u32,

	/// Sector Size of the volume. This will affect the file size.
	pub sector_size: u32,

	/// Optional Volume Security descriptor.
	///
	/// See [`InitializeSecurityDescriptor`].
	///
	/// [`InitializeSecurityDescriptor`]: https://docs.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-initializesecuritydescriptor
	pub volume_security_descriptor: Option<[SCHAR; VOLUME_SECURITY_DESCRIPTOR_MAX_SIZE]>,
}

impl Default for MountOptions<'_> {
	fn default() -> Self {
		Self {
			single_thread: Default::default(),
			flags: MountFlags::empty(),
			unc_name: Default::default(),
			timeout: Default::default(),
			allocation_unit_size: Default::default(),
			sector_size: Default::default(),
			volume_security_descriptor: Default::default(),
		}
	}
}

/// Error type for [`FileSystemMounter::mount`].
#[repr(i32)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FileSystemMountError {
	/// A general error.
	General = DOKAN_ERROR,

	/// Bad drive letter.
	DriveLetter = DOKAN_DRIVE_LETTER_ERROR,

	/// Can't install the Dokan driver.
	DriverInstall = DOKAN_DRIVER_INSTALL_ERROR,

	/// The driver responds that something is wrong.
	Start = DOKAN_START_ERROR,

	/// Can't assign a drive letter or mount point.
	///
	/// This probably means that the mount point is already used by another volume.
	Mount = DOKAN_MOUNT_ERROR,

	/// The mount point is invalid.
	MountPoint = DOKAN_MOUNT_POINT_ERROR,

	/// The Dokan version that this wrapper is targeting is incompatible with the loaded Dokan
	/// library.
	Version = DOKAN_VERSION_ERROR,
}

impl From<i32> for FileSystemMountError {
	fn from(value: i32) -> Self {
		Self::from_native(value).unwrap_or(Self::General)
	}
}

impl FileSystemMountError {
	fn from_native(value: i32) -> Option<Self> {
		match value {
			DOKAN_ERROR => Some(Self::General),
			DOKAN_DRIVE_LETTER_ERROR => Some(Self::DriveLetter),
			DOKAN_DRIVER_INSTALL_ERROR => Some(Self::DriverInstall),
			DOKAN_START_ERROR => Some(Self::Start),
			DOKAN_MOUNT_ERROR => Some(Self::Mount),
			DOKAN_MOUNT_POINT_ERROR => Some(Self::MountPoint),
			DOKAN_VERSION_ERROR => Some(Self::Version),
			_ => None,
		}
	}
}

impl Error for FileSystemMountError {}

impl Display for FileSystemMountError {
	fn fmt(&self, f: &mut Formatter) -> fmt::Result {
		let msg = match self {
			FileSystemMountError::General => "general error",
			FileSystemMountError::DriveLetter => "bad drive letter",
			FileSystemMountError::DriverInstall => "can't install driver",
			FileSystemMountError::Start => "the driver responds that something is wrong",
			FileSystemMountError::Mount => "can't assign a drive letter or mount point, probably already used by another volume",
			FileSystemMountError::MountPoint => "the mount point is invalid",
			FileSystemMountError::Version => "requested an incompatible version",
		};
		write!(f, "{}", msg)
	}
}

/// Error returned by [`FileSystemMounter::mount_with_cancellation`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum CancellableMountError {
	/// Mount startup and its native cleanup completed after cancellation was requested.
	Cancelled,
	/// The loaded native driver does not implement cancellable mount startup.
	DriverFeatureUnsupported,
	/// A mount error also returned by the legacy [`FileSystemMounter::mount`] path.
	Mount(FileSystemMountError),
	/// An unrecognized native result was returned without reinterpreting it as an enum.
	UnknownNativeResult(i32),
}

impl CancellableMountError {
	fn from_native(value: i32) -> Self {
		match value {
			DOKAN_CANCELLED_ERROR => Self::Cancelled,
			DOKAN_DRIVER_FEATURE_ERROR => Self::DriverFeatureUnsupported,
			_ => FileSystemMountError::from_native(value)
				.map(Self::Mount)
				.unwrap_or(Self::UnknownNativeResult(value)),
		}
	}
}

impl Error for CancellableMountError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Mount(error) => Some(error),
			_ => None,
		}
	}
}

impl Display for CancellableMountError {
	fn fmt(&self, f: &mut Formatter) -> fmt::Result {
		match self {
			Self::Cancelled => f.write_str("mount startup was cancelled and cleaned up"),
			Self::DriverFeatureUnsupported => {
				f.write_str("the loaded driver does not support cancellable mount startup")
			}
			Self::Mount(error) => Display::fmt(error, f),
			Self::UnknownNativeResult(value) => {
				write!(f, "unknown native mount result {value}")
			}
		}
	}
}

#[test]
fn maps_extended_mount_results_without_reinterpreting_unknown_values() {
	assert_eq!(
		FileSystemMountError::from(-12345),
		FileSystemMountError::General
	);
	assert_eq!(
		CancellableMountError::from_native(DOKAN_CANCELLED_ERROR),
		CancellableMountError::Cancelled
	);
	assert_eq!(
		CancellableMountError::from_native(DOKAN_DRIVER_FEATURE_ERROR),
		CancellableMountError::DriverFeatureUnsupported
	);
	assert_eq!(
		CancellableMountError::from_native(DOKAN_MOUNT_ERROR),
		CancellableMountError::Mount(FileSystemMountError::Mount)
	);
	assert_eq!(
		CancellableMountError::from_native(-12345),
		CancellableMountError::UnknownNativeResult(-12345)
	);
}

/// A mounter of [`FileSystem`].
pub struct FileSystemMounter<'c, 'h: 'c, FSH: FileSystemHandler<'c, 'h> + 'h> {
	options: DOKAN_OPTIONS,
	operations: DOKAN_OPERATIONS,
	phantom_handler: PhantomData<&'h FSH>,
	phantom_context: PhantomData<&'c FSH::Context>,
}

impl<'c, 'h: 'c, FSH: FileSystemHandler<'c, 'h> + 'h> FileSystemMounter<'c, 'h, FSH> {
	/// Creates a file system. It should be `mut`, as [`mount`](Self::mount) requires it.
	///
	/// # Arguments
	///
	/// * `handler` - Implements [`FileSystemHandler`].
	/// * `mount_point`- Can be a driver letter like `"M"` or a folder path `"C:\mount\dokan"` on a NTFS partition.
	/// * `options` - Customizes behavior.
	pub fn new(handler: &'h FSH, mount_point: &'h U16CStr, options: &'h MountOptions) -> Self {
		Self {
			options: DOKAN_OPTIONS {
				Version: WRAPPER_VERSION as u16,
				SingleThread: options.single_thread.into(),
				Options: options.flags.bits(),
				GlobalContext: handler as *const _ as u64,
				MountPoint: mount_point.as_ptr(),
				UNCName: match options.unc_name {
					Some(s) => s.as_ptr(),
					None => ptr::null(),
				},
				Timeout: options.timeout.as_millis() as u32,
				AllocationUnitSize: options.allocation_unit_size,
				SectorSize: options.sector_size,
				VolumeSecurityDescriptorLength: match options.volume_security_descriptor {
					Some(_) => VOLUME_SECURITY_DESCRIPTOR_MAX_SIZE as u32,
					None => 0,
				},
				VolumeSecurityDescriptor: options
					.volume_security_descriptor
					.unwrap_or([0; VOLUME_SECURITY_DESCRIPTOR_MAX_SIZE]),
			},
			operations: DOKAN_OPERATIONS {
				ZwCreateFile: Some(operations::create_file::<'c, 'h, FSH>),
				Cleanup: Some(operations::cleanup::<'c, 'h, FSH>),
				CloseFile: Some(operations::close_file::<'c, 'h, FSH>),
				ReadFile: Some(operations::read_file::<'c, 'h, FSH>),
				WriteFile: Some(operations::write_file::<'c, 'h, FSH>),
				FlushFileBuffers: Some(operations::flush_file_buffers::<'c, 'h, FSH>),
				GetFileInformation: Some(operations::get_file_information::<'c, 'h, FSH>),
				FindFiles: Some(operations::find_files::<'c, 'h, FSH>),
				FindFilesWithPattern: Some(operations::find_files_with_pattern::<'c, 'h, FSH>),
				SetFileAttributes: Some(operations::set_file_attributes::<'c, 'h, FSH>),
				SetFileTime: Some(operations::set_file_time::<'c, 'h, FSH>),
				DeleteFile: Some(operations::delete_file::<'c, 'h, FSH>),
				DeleteDirectory: Some(operations::delete_directory::<'c, 'h, FSH>),
				MoveFile: Some(operations::move_file::<'c, 'h, FSH>),
				SetEndOfFile: Some(operations::set_end_of_file::<'c, 'h, FSH>),
				SetAllocationSize: Some(operations::set_allocation_size::<'c, 'h, FSH>),
				LockFile: Some(operations::lock_file::<'c, 'h, FSH>),
				UnlockFile: Some(operations::unlock_file::<'c, 'h, FSH>),
				GetDiskFreeSpace: Some(operations::get_disk_free_space::<'c, 'h, FSH>),
				GetVolumeInformation: Some(operations::get_volume_information::<'c, 'h, FSH>),
				Mounted: Some(operations::mounted::<'c, 'h, FSH>),
				Unmounted: Some(operations::unmounted::<'c, 'h, FSH>),
				GetFileSecurity: Some(operations::get_file_security::<'c, 'h, FSH>),
				SetFileSecurity: Some(operations::set_file_security::<'c, 'h, FSH>),
				FindStreams: Some(operations::find_streams::<'c, 'h, FSH>),
			},
			phantom_handler: PhantomData,
			phantom_context: PhantomData,
		}
	}

	/// Mounts the file system. If successful, blocks the current thread until the file system gets unmounted.
	pub fn mount(&mut self) -> Result<FileSystem<'c, 'h, FSH>, FileSystemMountError> {
		let mut instance = ptr::null_mut();

		let result = unsafe {
			DokanCreateFileSystem(&mut self.options, &mut self.operations, &mut instance)
		};

		if result == DOKAN_SUCCESS {
			Ok(FileSystem {
				instance,
				_pin: PhantomData,
			})
		} else {
			Err(result.into())
		}
	}

	/// Mounts with a borrowed manual-reset event for cooperative startup cancellation.
	///
	/// The native call does not return [`CancellableMountError::Cancelled`] until startup
	/// cleanup is complete, so the event and this mounter stay borrowed for the whole
	/// operation. The event may be signalled from another thread.
	///
	/// Cancellation is cooperative across the native driver stack. If a lower driver
	/// does not complete a cancelled request, this call can remain blocked; use process
	/// isolation when that failure class requires a hard termination bound.
	pub fn mount_with_cancellation(
		&mut self,
		cancellation_event: BorrowedHandle<'_>,
	) -> Result<FileSystem<'c, 'h, FSH>, CancellableMountError> {
		let mut instance = ptr::null_mut();
		let result = unsafe {
			DokanCreateFileSystemEx(
				&mut self.options,
				&mut self.operations,
				cancellation_event.as_raw_handle().cast(),
				&mut instance,
			)
		};

		if result == DOKAN_SUCCESS {
			Ok(FileSystem {
				instance,
				_pin: PhantomData,
			})
		} else {
			Err(CancellableMountError::from_native(result))
		}
	}
}

/// A successfully mounted file system.
///
/// When dropped, the current thread will block until the file system gets unmounted.
pub struct FileSystem<'c, 'h: 'c, FSH: FileSystemHandler<'c, 'h> + 'h> {
	instance: DOKAN_HANDLE,
	_pin: PhantomData<&'h FileSystemMounter<'c, 'h, FSH>>,
}

impl<'c, 'h: 'c, FSH: FileSystemHandler<'c, 'h> + 'h> FileSystem<'c, 'h, FSH> {
	pub fn instance(&self) -> FileSystemHandle {
		FileSystemHandle(self.instance)
	}
}

impl<'c, 'h: 'c, FSH: FileSystemHandler<'c, 'h> + 'h> PartialEq for FileSystem<'c, 'h, FSH> {
	fn eq(&self, other: &Self) -> bool {
		self.instance == other.instance
	}
}

impl<'c, 'h: 'c, FSH: FileSystemHandler<'c, 'h> + 'h> Drop for FileSystem<'c, 'h, FSH> {
	fn drop(&mut self) {
		unsafe {
			DokanWaitForFileSystemClosed(self.instance, INFINITE);
			DokanCloseHandle(self.instance);
		}
	}
}

#[test]
fn can_fail_to_mount() {
	use std::sync::mpsc;

	use crate::{
		init, shutdown,
		usage_tests::{convert_str, TestHandler},
	};

	let (tx, _rx) = mpsc::sync_channel(1024);

	init();

	{
		let mount_point = convert_str("0");
		let handler = TestHandler::new(tx);
		let options = Default::default();
		let mut file_system = FileSystemMounter::new(&handler, &mount_point, &options);
		match file_system.mount() {
			Ok(_) => panic!("file system successfully mounted, but it should not"),
			Err(err) => assert_eq!(err, FileSystemMountError::Mount),
		};
	}

	shutdown();
}

/// A handle to a [`FileSystem`] instance, to be passed to `notify_*` functions.
///
/// Warning: because it is meant to be sent across threads, the handle bypasses its file system's lifetime.
/// Therefore, ensure you do not use it after the file system is unmounted.
#[derive(Clone, Copy)]
pub struct FileSystemHandle(pub(crate) DOKAN_HANDLE);

impl FileSystemHandle {
	/// Requests an unmount of this exact file system instance without waiting for it to close.
	///
	/// This does not look up the file system by its mount point. Use the owning [`FileSystem`]
	/// lifetime to ensure this handle is not used after the instance has closed.
	pub fn request_unmount(self) -> bool {
		unsafe { DokanRequestUnmount(self.0) != 0 }
	}
}

unsafe impl Send for FileSystemHandle {}
