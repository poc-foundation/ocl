//! Interfaces to a buffer.

use std;
use std::ops::{Deref, DerefMut, Range, RangeFull, Index, IndexMut};
use std::default::Default;
use std::slice::{Iter, IterMut};
use rand;
use rand::distributions::{IndependentSample, Range as RandRange};
use num::{FromPrimitive, ToPrimitive};

use core::{self, OclNum, Mem as MemCore, CommandQueue as CommandQueueCore, MemFlags, 
    MemInfo, MemInfoResult, ClEventPtrNew};
use util;
use error::{Error as OclError, Result as OclResult};
use standard::{Queue, BufferDims, EventList};

static VEC_OPT_ERR_MSG: &'static str = "No host side vector defined for this Buffer. \
    You must create this Buffer using 'Buffer::with_vec()' (et al.) in order to call this method.";



    // pub unsafe fn enqueue_read_buffer<T: OclNum, L: AsRef<EventList>, E: ClEventPtrNew>(
    //             command_queue: &CommandQueue,
    //             buffer: &Mem, 
    //             block: bool,
    //             offset: usize,
    //             data: &mut [T],
    //             wait_list: Option<&L>, 
    //             new_event: Option<&mut E>,

    // pub unsafe fn enqueue_read_buffer_rect<T: OclNum, L: AsRef<EventList>, E: ClEventPtrNew>(
    //             command_queue: &CommandQueue,
    //             buffer: &Mem, 
    //             block: bool,
    //             buffer_origin: [usize; 3],
    //             host_origin: [usize; 3],
    //             region: [usize; 3],
    //             buffer_row_pitch: usize,
    //             buffer_slc_pitch: usize,
    //             host_row_pitch: usize,
    //             host_slc_pitch: usize,
    //             data: &mut [T],
    //             wait_list: Option<&L>, 
    //             new_event: Option<&mut E>,

pub enum OpKind<'b, T: 'b> {
    Unspecified,
    Read { data: &'b mut [T] },
    Write { data: &'b [T] },
    Copy { dst_buffer: &'b MemCore },
    Fill { pattern: &'b [T], pattern_size: usize },
    CopyToImage { image: &'b MemCore, dst_origin: [usize; 3], region: [usize; 3] },
} 

impl<'b, T: 'b> OpKind<'b, T> {
    fn is_unspec(&'b self) -> bool {
        if let &OpKind::Unspecified = self {
            true
        } else {
            false
        }
    }
}

pub enum OpShape {
    Lin { offset: usize },
    Rect { 
        src_origin: [usize; 3],
        dst_origin: [usize; 3],
        region: [usize; 3],
        src_row_pitch: usize,
        src_slc_pitch: usize,
        dst_row_pitch: usize,
        dst_slc_pitch: usize,
    },
}

/// A kernel command builder used to queue a kernel with a mix of default
/// and optionally specified arguments.
pub struct BufferCmd<'b, T: 'b + OclNum> {
    queue: &'b CommandQueueCore,
    obj_core: &'b MemCore,
    block: bool,
    lock_block: bool,
    kind: OpKind<'b, T>,
    shape: OpShape,    
    wait_list: Option<&'b EventList>,
    dest_list: Option<&'b mut ClEventPtrNew>,
    mem_len: usize,
}

impl<'b, T: 'b + OclNum> BufferCmd<'b, T> {
    /// Returns a new buffer command builder associated with with the
    /// memory object `obj_core` along with a default `queue` and `mem_len` 
    /// (the length of the device side buffer).
    fn new(queue: &'b CommandQueueCore, obj_core: &'b MemCore, mem_len: usize) 
            -> BufferCmd<'b, T>
    {
        BufferCmd {
            queue: queue,
            obj_core: obj_core,
            block: true,
            lock_block: false,
            kind: OpKind::Unspecified,
            shape: OpShape::Lin { offset: 0 },
            wait_list: None,
            dest_list: None,
            mem_len: mem_len,
        }
    }

    /// Specifies a queue to use for this call only.
    pub fn queue(mut self, queue: &'b Queue) -> BufferCmd<'b, T> {
        self.queue = queue.core_as_ref();
        self
    }

    /// Specifies whether or not to block thread until completion.
    ///
    /// # Panics
    ///
    /// Will panic if `::read` has already been called. Use `::read_async`
    /// for a non-blocking read operation.
    ///
    pub fn block(mut self, block: bool) -> BufferCmd<'b, T> {
        assert!(!self.lock_block, "ocl::BufferCmd::block(): Blocking for this command has been \
            disabled by the '::read' method. For non-blocking reads use '::read_async'.");
        self.block = block;
        self
    }

    /// Sets the linear offset for an operation.
    /// 
    /// # Panics
    ///
    /// The 'shape' may not have already been set to rectangular by the 
    /// `::rect` function.
    pub fn offset(mut self, offset: usize)  -> BufferCmd<'b, T> {
        if let OpShape::Rect { .. } = self.shape {
            panic!("ocl::BufferCmd::offset(): This command builder has already been set to \
                rectangular mode with '::rect`. You cannot call both '::offset' and '::rect'.");
        }

        self.shape = OpShape::Lin { offset: offset };
        self
    }

    /// Specifies that this command will be a blocking read operation.
    ///
    /// After calling this method, the blocking state of this command will
    /// be locked to true and a call to `::block` will cause a panic.
    ///
    /// # Panics
    ///
    /// The command operation kind must not have already been specified.
    ///
    pub fn read(mut self, dst_data: &'b mut [T]) -> BufferCmd<'b, T> {
        assert!(self.kind.is_unspec(), "ocl::BufferCmd::read(): Operation kind \
            already set for this command.");
        self.kind = OpKind::Read { data: dst_data };
        self.block = true;
        self.lock_block = true;
        self
    }

    /// Specifies that this command will be a non-blocking, asynchronous read
    /// operation.
    ///
    /// Sets the block mode to false automatically but it may still be freely
    /// toggled back. If set back to `true` this method call becomes equivalent
    /// to calling `::read`.
    ///
    /// ## Safety
    ///
    /// Caller must ensure that the container referred to by `dst_data` lives 
    /// until the call completes.
    ///
    /// # Panics
    ///
    /// The command operation kind must not have already been specified
    ///
    pub unsafe fn read_async(mut self, dst_data: &'b mut [T]) -> BufferCmd<'b, T> {
        assert!(self.kind.is_unspec(), "ocl::BufferCmd::read(): Operation kind \
            already set for this command.");
        self.kind = OpKind::Read { data: dst_data };
        self
    }

    /// Specifies that this command will be a write operation.
    ///
    /// # Panics
    ///
    /// The command operation kind must not have already been specified
    ///
    pub fn write(mut self, src_data: &'b [T]) -> BufferCmd<'b, T> {
        assert!(self.kind.is_unspec(), "ocl::BufferCmd::write(): Operation kind \
            already set for this command.");
        self.kind = OpKind::Write { data: src_data };
        self
    }

    /// Specifies that this command will be a copy operation.
    ///
    /// If `.block(..)` has been set it will be ignored.
    ///
    /// # Panics
    ///
    /// The command operation kind must not have already been specified
    ///
    pub fn copy(mut self, dst_buffer: &'b Buffer<T>) -> BufferCmd<'b, T> {
        assert!(self.kind.is_unspec(), "ocl::BufferCmd::copy(): Operation kind \
            already set for this command.");
        self.kind = OpKind::Copy { dst_buffer: dst_buffer.core_as_ref() }; 
        self
    }

    /// Specifies that this command will be a copy to image.
    ///
    /// If `.block(..)` has been set it will be ignored.
    ///
    /// # Panics
    ///
    /// The command operation kind must not have already been specified
    ///
    pub fn copy_to_image(mut self, image: &'b MemCore, dst_origin: [usize; 3], 
                region: [usize; 3]) -> BufferCmd<'b, T> 
    {
        assert!(self.kind.is_unspec(), "ocl::BufferCmd::copy_to_image(): Operation kind \
            already set for this command.");
        self.kind = OpKind::CopyToImage { image: image, dst_origin: dst_origin, region: region }; 
        self
    }

    /// Specifies that this command will be a fill.
    ///
    /// If `.block(..)` has been set it will be ignored.
    ///
    /// # Panics
    ///
    /// The command operation kind must not have already been specified
    ///
    pub fn fill(mut self, pattern: &'b [T], pattern_size: usize) -> BufferCmd<'b, T> {
        assert!(self.kind.is_unspec(), "ocl::BufferCmd::fill(): Operation kind \
            already set for this command.");
        self.kind = OpKind::Fill { pattern: pattern, pattern_size: pattern_size }; 
        self
    }

    /// Specifies that this will be a rectangularly shaped operation 
    /// (the default being linear).
    ///
    /// Only valid for 'read', 'write', and 'copy' modes. Will error if used
    /// with 'fill' or 'copy to image'.
    pub fn rect(mut self, src_origin: [usize; 3], dst_origin: [usize; 3], region: [usize; 3],
                src_row_pitch: usize, src_slc_pitch: usize, dst_row_pitch: usize, 
                dst_slc_pitch: usize) -> BufferCmd<'b, T>
    {
        if let OpShape::Lin { offset } = self.shape {
            assert!(offset == 0, "ocl::BufferCmd::rect(): This command builder has already been \
                set to linear mode with '::offset`. You cannot call both '::offset' and '::rect'.");
        }

        self.shape = OpShape::Rect { src_origin: src_origin, dst_origin: dst_origin,
            region: region, src_row_pitch: src_row_pitch, src_slc_pitch: src_slc_pitch,
            dst_row_pitch: dst_row_pitch, dst_slc_pitch: dst_slc_pitch };
        self
    }

    /// Specifies a list of events to wait on before the command will run.
    pub fn ewait(mut self, wait_list: &'b EventList) -> BufferCmd<'b, T> {
        self.wait_list = Some(wait_list);
        self
    }

    /// Specifies a list of events to wait on before the command will run or
    /// resets it to `None`.
    pub fn ewait_opt(mut self, wait_list: Option<&'b EventList>) -> BufferCmd<'b, T> {
        self.wait_list = wait_list;
        self
    }

    /// Specifies the destination for a new, optionally created event
    /// associated with this command.
    pub fn enew(mut self, dest_list: &'b mut ClEventPtrNew) -> BufferCmd<'b, T> {
        self.dest_list = Some(dest_list);
        self
    }

    /// Specifies a destination for a new, optionally created event
    /// associated with this command or resets it to `None`.
    pub fn enew_opt(mut self, dest_list: Option<&'b mut ClEventPtrNew>) -> BufferCmd<'b, T> {
        self.dest_list = dest_list;
        self
    }

    /// Enqueues this command.
    pub fn enq(self) -> OclResult<()> {
        match self.kind {
            OpKind::Read { data } => { 
                match self.shape {
                    OpShape::Lin { offset } => {                        
                        try!(check_len(self.mem_len, data.len(), offset));

                        unsafe { core::enqueue_read_buffer(self.queue, self.obj_core, self.block, 
                            offset, data, self.wait_list, self.dest_list) }
                    },
                    _ => unimplemented!(),
                }
            },
            OpKind::Write { data } => {
                match self.shape {
                    OpShape::Lin { offset } => {
                        try!(check_len(self.mem_len, data.len(), offset));
                        core::enqueue_write_buffer(self.queue, self.obj_core, self.block, 
                            offset, data, self.wait_list, self.dest_list)
                    },
                    _ => unimplemented!(),
                }
            },
            OpKind::Unspecified => return OclError::err("ocl::BufferCmd::enq(): No operation \
                specified. Use '.read(...)', 'write(...)', etc. before calling '.enq()'."),
            _ => unimplemented!(),
        }
    }
}

fn check_len(mem_len: usize, data_len: usize, offset: usize) -> OclResult<()> {
    if offset >= mem_len { return OclError::err(
        "ocl::Buffer::enq(): Offset out of range."); }
    if data_len > (mem_len - offset) { return OclError::err(
        "ocl::Buffer::enq(): Data length exceeds buffer length."); }
    Ok(())
}






// An option type mainly just for convenient error handling.
#[derive(Debug, Clone)]
enum VecOption<T> {
    None,
    Some(Vec<T>),
}

impl<T> VecOption<T> {
    fn as_ref(&self) -> OclResult<&Vec<T>> {
        match self {
            &VecOption::Some(ref vec) => {
                Ok(vec)
            },
            &VecOption::None => Err(OclError::new(VEC_OPT_ERR_MSG)),
        }
    }

    fn as_mut(&mut self) -> OclResult<&mut Vec<T>> {
        match self {
            &mut VecOption::Some(ref mut vec) => {
                Ok(vec)
            },
            &mut VecOption::None => Err(OclError::new(VEC_OPT_ERR_MSG)),
        }
    }

    fn is_some(&self) -> bool {
        if let &VecOption::None = self { false } else { true }
    }
}




/// A buffer with an optional built-in vector. 
///
/// Data is stored both remotely in a memory buffer on the device associated with 
/// `queue` and optionally in a vector (`self.vec`) in host (local) memory for 
/// convenient use as a workspace etc.
///
/// The host side vector must be manually synchronized with the device side buffer 
/// using `::fill_vec`, `::flush_vec`, etc. Data within the contained vector 
/// should generally be considered stale except immediately after a fill/flush 
/// (exception: pinned memory).
///
/// Fill/flush methods are for convenience and are equivalent to the psuedocode: 
/// read_into(self.vec) and write_from(self.vec).
///
/// # Stability
///
/// Read/write/fill/flush methods will eventually be returning a result instead of
/// panicing upon error.
///
// TODO: Return result for reads and writes instead of panicing.
// TODO: Check that type size (sizeof(T)) is <= the maximum supported by device.
// TODO: Consider integrating an event list to help coordinate pending reads/writes.
#[derive(Debug, Clone)]
pub struct Buffer<T: OclNum> {
    // vec: Vec<T>,
    obj_core: MemCore,
    // queue: Queue,
    command_queue_obj_core: CommandQueueCore,
    len: usize,
    vec: VecOption<T>,
}

///
/// # Panics
/// All methods will panic upon any OpenCL error.
impl<T: OclNum> Buffer<T> {
    /// Creates a new read/write Buffer with dimensions: `dims` which will use the 
    /// command queue: `queue` (and its associated device and context) for all operations.
    ///
    /// The device side buffer will be allocated a size based on the maximum workgroup 
    /// size of the device. This helps ensure that kernels do not attempt to read 
    /// from or write to memory beyond the length of the buffer (see crate level 
    /// documentation for more details about how dimensions are used). The buffer
    /// will be initialized with a sensible default value (probably `0`).
    ///
    /// # Other Method Panics
    /// The returned Buffer contains no host side vector. Functions associated with
    /// one such as `.enqueue_flush_vec()`, `enqueue_fill_vec()`, etc. will panic.
    /// [FIXME]: Return result.
    pub fn new<D: BufferDims>(dims: D, queue: &Queue) -> Buffer<T> {
        let len = dims.padded_buffer_len(queue.device().max_wg_size());
        Buffer::_new(len, queue)
    }

    /// Creates a new read/write Buffer with a host side working copy of data.
    /// Host vector and device buffer are initialized with a sensible default value.
    /// [FIXME]: Return result.
    pub fn with_vec<D: BufferDims>(dims: D, queue: &Queue) -> Buffer<T> {
        let len = dims.padded_buffer_len(queue.device().max_wg_size());
        let vec: Vec<T> = std::iter::repeat(T::default()).take(len).collect();

        Buffer::_with_vec(vec, queue)
    }

    /// [UNSTABLE]: Convenience method.
    /// Creates a new read/write Buffer with a host side working copy of data.
    /// Host vector and device buffer are initialized with the value, `init_val`.
    /// [FIXME]: Return result.
    pub fn with_vec_initialized_to<D: BufferDims>(init_val: T, dims: D, queue: &Queue) -> Buffer<T> {
        let len = dims.padded_buffer_len(queue.device().max_wg_size());
        let vec: Vec<T> = std::iter::repeat(init_val).take(len).collect();

        Buffer::_with_vec(vec, queue)
    }

    /// [UNSTABLE]: Convenience method.
    /// Creates a new read/write Buffer with a vector initialized with a series of 
    /// integers ranging from `vals.0` to `vals.1` (closed) which are shuffled 
    /// randomly.
    ///
    /// Note: Even if the Buffer type is a floating point type, the values returned
    /// will still be integral values (e.g.: 1.0, 2.0, 3.0, etc.).
    ///
    /// # Security
    ///
    /// Resulting values are not cryptographically secure.
    /// [FIXME]: Return result.
    // Note: vals.1 is inclusive.
    pub fn with_vec_shuffled<D: BufferDims>(vals: (T, T), dims: D, queue: &Queue) 
            -> Buffer<T> 
    {
        let len = dims.padded_buffer_len(queue.device().max_wg_size());
        let vec: Vec<T> = shuffled_vec(len, vals);

        Buffer::_with_vec(vec, queue)
    }

    /// [UNSTABLE]: Convenience method.
    /// Creates a new read/write Buffer with a vector initialized with random values 
    /// within the (half-open) range `vals.0..vals.1`.
    ///
    /// # Security
    ///
    /// Resulting values are not cryptographically secure.
    /// [FIXME]: Return result.
    // Note: vals.1 is exclusive.
    pub fn with_vec_scrambled<D: BufferDims>(vals: (T, T), dims: D, queue: &Queue) 
            -> Buffer<T> 
    {
        let len = dims.padded_buffer_len(queue.device().max_wg_size());
        let vec: Vec<T> = scrambled_vec(len, vals);

        Buffer::_with_vec(vec, queue)
    }   

    /// Creates a new Buffer with caller-managed buffer length, type, flags, and 
    /// initialization.
    ///
    /// # Examples
    /// See `examples/buffer_unchecked.rs`.
    ///
    /// # Parameter Reference Documentation
    /// Refer to the following page for information about how to configure flags and
    /// other options for the buffer.
    /// [https://www.khronos.org/registry/cl/sdk/1.2/docs/man/xhtml/clCreateBuffer.html](https://www.khronos.org/registry/cl/sdk/1.2/docs/man/xhtml/clCreateBuffer.html)
    ///
    /// # Safety
    /// No creation time checks are made to help prevent device buffer overruns, 
    /// etc. Caller should be sure to initialize the device side memory with a 
    /// write. Any host side memory pointed to by `host_ptr` is at even greater
    /// risk if used with `CL_MEM_USE_HOST_PTR` (see below).
    ///
    /// [IMPORTANT] Practically every read and write to an Buffer created in this way is
    /// potentially unsafe. Because `.enqueue_read()` and `.enqueue_write()` do not require an 
    /// unsafe block, their implied promises about safety may be broken at any time.
    ///
    /// *You need to know what you're doing and be extra careful using an Buffer created 
    /// with this method because it badly breaks Rust's usual safety promises even
    /// outside of an unsafe block.*
    ///
    /// **This is horribly un-idiomatic Rust. You have been warned.**
    ///
    /// NOTE: The above important warnings probably only apply to buffers created with 
    /// the `CL_MEM_USE_HOST_PTR` flag because the memory is considered 'pinned' but 
    /// there may also be implementation specific issues which haven't been considered 
    /// or are unknown.
    ///
    /// [FIXME]: Return result.
    pub unsafe fn new_unchecked(flags: MemFlags, len: usize, host_ptr: Option<&[T]>, 
                queue: &Queue) -> Buffer<T> 
    {
        let obj_core = core::create_buffer(queue.context_core_as_ref(), flags, len,
            host_ptr).expect("[FIXME: TEMPORARY]: Buffer::_new():");

        Buffer {
            obj_core: obj_core,
            command_queue_obj_core: queue.core_as_ref().clone(),
            len: len,
            vec: VecOption::None,
        }
    }

    // Consolidated constructor for Buffers without vectors.
    /// [FIXME]: Return result.
    fn _new(len: usize, queue: &Queue) -> Buffer<T> {
        let obj_core = unsafe { core::create_buffer::<T>(queue.context_core_as_ref(),
            core::MEM_READ_WRITE, len, None).expect("Buffer::_new()") };

        Buffer {            
            obj_core: obj_core,
            command_queue_obj_core: queue.core_as_ref().clone(),
            len: len,
            vec: VecOption::None,
        }
    }

    // Consolidated constructor for Buffers with vectors.
    /// [FIXME]: Return result.
    fn _with_vec(mut vec: Vec<T>, queue: &Queue) -> Buffer<T> {
        let obj_core = unsafe { core::create_buffer(queue.context_core_as_ref(), 
            core::MEM_READ_WRITE | core::MEM_COPY_HOST_PTR, vec.len(), Some(&mut vec))
            .expect("Buffer::_with_vec()") };

        Buffer {        
            obj_core: obj_core,
            command_queue_obj_core: queue.core_as_ref().clone(),
            len: vec.len(), 
            vec: VecOption::Some(vec),
        }
    }


    /// Returns a buffer command builder used to read, write, copy, etc.
    ///
    /// Run `.enq()` to enqueue the command.
    ///
    pub fn cmd<'b>(&'b self) -> BufferCmd<'b, T> {
        BufferCmd::new(&self.command_queue_obj_core, &self.obj_core, self.len)
    }


    /// Enqueues reading `data.len() * mem::size_of::<T>()` bytes from the device 
    /// buffer into `data` with a remote offset of `offset`.
    ///
    /// Setting `queue` to `None` will use the default queue set during creation.
    /// Otherwise, the queue passed will be used for this call only.
    ///
    /// Will optionally wait for events in `wait_list` to finish 
    /// before reading. Will also optionally add a new event associated with
    /// the read to `dest_list`.
    ///
    /// [UPDATE] If the `dest_list` event list is `None`, the read will be blocking, otherwise
    /// returns immediately.
    ///
    /// # Safety
    ///
    /// Bad things will happen if the memory referred to by `data` is freed and
    /// reallocated before the read completes. It's up to the caller to make 
    /// sure that the new event added to `dest_list` completes. Use 
    /// 'dest_list.last()' right after the calling `::read_async` to get a.
    /// reference to the event associated with the read. [NOTE: Improved ease
    /// of use is coming to the event api eventually]
    ///
    /// # Errors
    ///
    /// `offset` must be less than the length of the buffer.
    ///
    /// The length of `data` must be less than the length of the buffer minus `offset`.
    ///
    /// Errors upon any OpenCL error.
    ///
    /// [UNSTABLE: Likely to be depricated in favor of `::cmd`.
    pub unsafe fn enqueue_read(&self, queue: Option<&Queue>, block: bool, offset: usize, data: &mut [T],
                wait_list: Option<&EventList>, dest_list: Option<&mut ClEventPtrNew>) -> OclResult<()>
    {
        // assert!(offset < self.len(), "Buffer::read{{_async}}(): Offset out of range.");
        // assert!(data.len() <= self.len() - offset, 
        //     "Buffer::read{{_async}}(): Data length out of range.");
        if offset >= self.len() { 
            return OclError::err("Buffer::read{{_async}}(): Offset out of range."); }
        if data.len() > self.len() - offset {
            return OclError::err("Buffer::read{{_async}}(): Data length out of range."); }

        let command_queue = match queue {
            Some(q) => q.core_as_ref(),
            None => &self.command_queue_obj_core,
        };

        // let blocking_read = dest_list.is_none();
        core::enqueue_read_buffer(command_queue, &self.obj_core, block, 
            // offset, data, wait_list.map(|el| el.core_as_ref()), dest_list.map(|el| el.core_as_mut()))
            offset, data, wait_list, dest_list)
    }

    /// Enqueues writing `data.len() * mem::size_of::<T>()` bytes from `data` to the 
    /// device buffer with a remote offset of `offset`.
    ///
    /// Setting `queue` to `None` will use the default queue set during creation.
    /// Otherwise, the queue passed will be used for this call only.
    ///
    /// Will optionally wait for events in `wait_list` to finish before writing. 
    /// Will also optionally add a new event associated with the write to `dest_list`.
    ///
    /// [UPDATE] If the `dest_list` event list is `None`, the write will be blocking, otherwise
    /// returns immediately.
    ///
    /// # Data Integrity
    ///
    /// Ensure that the memory referred to by `data` is unmolested until the 
    /// write completes if passing a `dest_list`.
    ///
    /// # Errors
    ///
    /// `offset` must be less than the length of the buffer.
    ///
    /// The length of `data` must be less than the length of the buffer minus `offset`.
    ///
    /// Errors upon any OpenCL error.
    /// 
    /// [UNSTABLE: Likely to be depricated in favor of `::cmd`.
    pub fn enqueue_write(&self, queue: Option<&Queue>, block: bool, offset: usize, data: &[T], 
                wait_list: Option<&EventList>, dest_list: Option<&mut ClEventPtrNew>) -> OclResult<()>
    {        
        // assert!(offset < self.len(), "Buffer::write{{_async}}(): Offset out of range.");
        // assert!(data.len() <= self.len() - offset, 
        //     "Buffer::write{{_async}}(): Data length out of range.");
        if offset >= self.len() { 
            return OclError::err("Buffer::write{{_async}}(): Offset out of range."); }
        if data.len() > self.len() - offset {
            return OclError::err("Buffer::write{{_async}}(): Data length out of range."); }

        let command_queue = match queue {
            Some(q) => q.core_as_ref(),
            None => &self.command_queue_obj_core,
        };

        // let blocking_write = dest_list.is_none();
        core::enqueue_write_buffer(command_queue, &self.obj_core, block, 
            // offset, data, wait_list.map(|el| el.core_as_ref()), dest_list.map(|el| el.core_as_mut()))
            offset, data, wait_list, dest_list)
    }

    /// Reads `data.len() * mem::size_of::<T>()` bytes from the (remote) device buffer 
    /// into `data` with a remote offset of `offset` and blocks until complete.
    ///
    /// # Errors
    ///
    /// `offset` must be less than the length of the buffer.
    ///
    /// The length of `data` must be less than the length of the buffer minus `offset`.
    ///
    /// Errors upon any OpenCL error.
    pub fn read(&self, data: &mut [T], offset: usize) -> OclResult<()>
    {
        // Safe due to being a blocking read (right?).
        unsafe { self.enqueue_read(None, true, offset, data, None, None) }
    }

    /// Writes `data.len() * mem::size_of::<T>()` bytes from `data` to the (remote) 
    /// device buffer with a remote offset of `offset` and blocks until complete.
    ///
    /// # Panics
    ///
    /// `offset` must be less than the length of the buffer.
    ///
    /// The length of `data` must be less than the length of the buffer minus `offset`.
    ///
    /// Errors upon any OpenCL error.
    pub fn write(&self, data: &[T], offset: usize) -> OclResult<()>
    {
        self.enqueue_write(None, true, offset, data, None, None)
    }

    /// After waiting on events in `wait_list` to finish, reads the remote device 
    /// data buffer into 'self.vec' and adds a new event to `dest_list`.
    ///
    /// [UPDATE] Will block until the read is complete and the internal vector is filled if 
    /// `dest_list` is `None`.
    ///
    /// # Safety 
    ///
    /// Currently up to the caller to ensure this `Buffer` lives long enough
    /// for the read to complete.
    ///
    /// TODO: Keep an internal eventlist to track pending reads and cancel them
    /// if this `Buffer` is destroyed beforehand.
    ///
    /// # Errors
    ///
    /// Errors if this Buffer contains no vector or upon any OpenCL error.
    pub unsafe fn enqueue_fill_vec(&mut self, block: bool, wait_list: Option<&EventList>, 
                dest_list: Option<&mut ClEventPtrNew>) -> OclResult<()>
    {
        debug_assert!(self.vec.as_ref().unwrap().len() == self.len());
        let vec = try!(self.vec.as_mut());
        core::enqueue_read_buffer(&self.command_queue_obj_core, &self.obj_core, block, 
            // 0, vec, wait_list.map(|el| el.core_as_ref()), dest_list.map(|el| el.core_as_mut()))
            0, vec, wait_list, dest_list)
    }

    /// Reads the remote device data buffer into `self.vec` and blocks until completed.
    ///
    /// Equivalent to `.enqueue_fill_vec(true, None, None)`.
    ///
    /// # Panics
    ///
    /// Panics if this Buffer contains no vector or upon any OpenCL error.
    pub fn fill_vec(&mut self) {
        // Safe due to being a blocking read (right?).
        unsafe { self.enqueue_fill_vec(true, None, None).expect("Buffer::fill_vec()"); }
    } 

    /// After waiting on events in `wait_list` to finish, writes the contents of
    /// 'self.vec' to the remote device data buffer and adds a new event to `dest_list`.
    ///
    /// # Data Integrity
    ///
    /// Ensure that this `Buffer` lives until until the write completes if 
    /// passing a `dest_list`.
    ///
    /// [UPDATE] Will block until the write is complete if `dest_list` is None.
    ///
    /// # Errors
    ///
    /// Errors if this Buffer contains no vector or upon any OpenCL error.
    pub fn enqueue_flush_vec(&mut self, block: bool, wait_list: Option<&EventList>, 
                dest_list: Option<&mut ClEventPtrNew>) -> OclResult<()>
    {
        debug_assert!(self.vec.as_ref().unwrap().len() == self.len());
        let vec = try!(self.vec.as_mut());
        core::enqueue_write_buffer(&self.command_queue_obj_core, &self.obj_core, block, 
            // 0, vec, wait_list.map(|el| el.core_as_ref()), dest_list.map(|el| el.core_as_mut()))
            0, vec, wait_list, dest_list)
    }

    /// Writes the contents of `self.vec` to the remote device data buffer and 
    /// blocks until completed. 
    ///
    /// Equivalent to `.enqueue_flush_vec(true, None, None)`.
    ///
    /// # Panics
    ///
    /// Panics if this Buffer contains no vector or upon any OpenCL error.
    pub fn flush_vec(&mut self) {
        self.enqueue_flush_vec(true, None, None).expect("Buffer::flush_vec()");
    }  

    /// Blocks the current thread until the underlying command queue has
    /// completed all commands.
    pub fn wait(&self) {
        core::finish(&self.command_queue_obj_core).unwrap();
    }

    /// [UNSTABLE]: Convenience method.
    ///
    /// # Panics [UPDATE ME]
    /// Panics if this Buffer contains no vector.
    /// [FIXME]: GET WORKING EVEN WITH NO CONTAINED VECTOR
    /// TODO: Consider adding to `BufferCmd`.
    pub fn set_all_to(&mut self, val: T) -> OclResult<()> {
        {
            let vec = try!(self.vec.as_mut());
            for ele in vec.iter_mut() {
                *ele = val;
            }
        }

        self.enqueue_flush_vec(true, None, None)
    }

    /// [UNSTABLE]: Convenience method.
    ///
    /// # Panics [UPDATE ME]
    ///
    /// Panics if this Buffer contains no vector.
    ///
    /// [FIXME]: GET WORKING EVEN WITH NO CONTAINED VECTOR
    /// TODO: Consider adding to `BufferCmd`.
    pub fn set_range_to(&mut self, val: T, range: Range<usize>) -> OclResult<()> {       
        {
            let vec = try!(self.vec.as_mut());
            // for idx in range {
                // self.vec[idx] = val;
            for ele in vec[range].iter_mut() {
                *ele = val;
            }
        }

        self.enqueue_flush_vec(true, None, None)
    }

    /// Returns the length of the Buffer.
    ///
    /// This is the length of both the device side buffer and the host side vector,
    /// if any. This may not agree with desired dataset size because it will have been
    /// rounded up to the nearest maximum workgroup size of the device on which it was
    /// created.
    #[inline]
    pub fn len(&self) -> usize {
        debug_assert!((if let VecOption::Some(ref vec) = self.vec { vec.len() } 
            else { self.len }) == self.len);
        self.len
    }

    /// Resizes Buffer. Recreates device side buffer and dangles any references 
    /// kernels may have had to the old buffer.
    ///
    /// # Safety
    ///
    /// [IMPORTANT]: You must manually reassign any kernel arguments which may have 
    /// had a reference to the (device side) buffer associated with this Buffer.
    /// [FIXME]: Return result.
    pub unsafe fn resize<B: BufferDims>(&mut self, new_dims: &B, queue: &Queue) {
        // self.release();
        let new_len = new_dims.padded_buffer_len(queue.device().max_wg_size());

        match self.vec {
            VecOption::Some(ref mut vec) => {
                vec.resize(new_len, T::default());
                self.obj_core = core::create_buffer(queue.context_core_as_ref(), 
                    core::MEM_READ_WRITE | core::MEM_COPY_HOST_PTR, self.len, Some(vec))
                    .expect("[FIXME: TEMPORARY]: Buffer::_resize()");
            },
            VecOption::None => {
                self.len = new_len;
                // let vec: Vec<T> = std::iter::repeat(T::default()).take(new_len).collect();
                self.obj_core = core::create_buffer::<T>(queue.context_core_as_ref(), 
                    core::MEM_READ_WRITE, self.len, None)
                    .expect("[FIXME: TEMPORARY]: Buffer::_resize()");
            },
        };
    }

    // /// Decrements the reference count associated with the previous buffer object, 
    // /// `self.obj_core`.
    // pub fn release(&mut self) {
    //     core::release_mem_object(self.obj_core).unwrap();
    // }

    /// Returns a reference to the local vector associated with this buffer.
    ///
    /// Contents of this vector may change during use due to previously enqueued
    /// reads. ([FIXME]: Is this a safety issue?)
    ///
    /// # Failures
    ///
    /// [FIXME: UPDATE DOC] Returns an error if this buffer contains no vector.
    #[inline]
    pub fn vec(&self) -> &Vec<T> {
        self.vec.as_ref().expect("Buffer::vec()")
    }

    /// Returns a mutable reference to the local vector associated with this buffer.
    ///
    /// Contents of this vector may change during use due to previously enqueued
    /// read.
    /// 
    /// # Failures
    ///
    /// [FIXME: UPDATE DOC] Returns an error if this buffer contains no vector.
    ///
    /// # Safety
    ///
    /// Could cause data collisions, etc. May not be unsafe strictly speaking
    /// (is it?) but marked as such to alert the caller to any potential 
    /// synchronization issues from previously enqueued reads.
    #[inline]
    pub unsafe fn vec_mut(&mut self) -> &mut Vec<T> {
        self.vec.as_mut().expect("Buffer::vec_mut()")
    }

    /// Returns an immutable reference to the value located at index `idx`, bypassing 
    /// bounds and enum variant checks.
    ///
    /// # Safety
    ///
    /// Assumes `self.vec` is a `VecOption::Vec` and that the index `idx` is within
    /// the vector bounds.
    #[inline]
    pub unsafe fn get_unchecked(&self, idx: usize) -> &T {
        debug_assert!(self.vec.is_some() && idx < self.len);
        let vec_ptr: *const Vec<T> = &self.vec as *const VecOption<T> as *const Vec<T>;
        (*vec_ptr).get_unchecked(idx) 
    }

    /// Returns a mutable reference to the value located at index `idx`, bypassing 
    /// bounds and enum variant checks.
    ///
    /// # Safety
    ///
    /// Assumes `self.vec` is a `VecOption::Vec` and that the index `idx` is within
    /// bounds. Can eat all the laundry.
    #[inline]
    pub unsafe fn get_unchecked_mut(&mut self, idx: usize) -> &mut T {      
        debug_assert!(self.vec.is_some() && idx < self.len);
        let vec_ptr_mut: *mut Vec<T> = &mut self.vec as *mut VecOption<T> as *mut Vec<T>;
        (*vec_ptr_mut).get_unchecked_mut(idx) 
    }

    /// Returns a copy of the core buffer object reference.
    pub fn core_as_ref(&self) -> &MemCore {
        &self.obj_core
    }

    /// Changes the default queue used by this Buffer for reads and writes, etc.
    ///
    /// Returns a ref for chaining i.e.:
    ///
    /// `buffer.set_queue(queue).flush_vec(....);`
    ///
    /// [NOTE]: Even when used as above, the queue is changed permanently,
    /// not just for the one call. Changing the queue is cheap so feel free
    /// to change as often as needed.
    ///
    pub fn set_queue<'a>(&'a mut self, queue: &Queue) -> &'a mut Buffer<T> {
        // [FIXME]: Set this up:
        // assert!(queue.device == self.queue.device);
        // [/FIXME]

        self.command_queue_obj_core = queue.core_as_ref().clone();
        self
    }

    /// Returns an iterator to a contained vector.
    ///
    /// # Panics
    ///
    /// Panics if this Buffer contains no vector.
    pub fn iter<'a>(&'a self) -> Iter<'a, T> {
        self.vec.as_ref().expect("Buffer::iter()").iter()
    }

    /// Returns a mutable iterator to a contained vector.
    ///
    /// # Panics
    ///
    /// Panics if this Buffer contains no vector.
    pub fn iter_mut<'a>(&'a mut self) -> IterMut<'a, T> {
        self.vec.as_mut().expect("Buffer::iter()").iter_mut()
    }


    /// [UNSTABLE]: Convenience method.
    ///
    /// # Panics
    ///
    /// Panics if this Buffer contains no vector.
    /// [FIXME]: GET WORKING EVEN WITH NO CONTAINED VECTOR
    pub fn print_simple(&mut self) {
        self.print(1, None, None, true);
    }

    /// [UNSTABLE]: Convenience method. 
    ///
    /// # Panics
    ///
    /// Panics if this Buffer contains no vector.
    /// [FIXME]: GET WORKING EVEN WITH NO CONTAINED VECTOR
    pub fn print_val_range(&mut self, every: usize, val_range: Option<(T, T)>,) {
        self.print(every, val_range, None, true);
    }

    /// [UNSTABLE]: Convenience/debugging method. May be moved/renamed/deleted.
    /// [FIXME]: CREATE AN EMPTY VECTOR FOR PRINTING IF NONE EXISTS INSTEAD
    /// OF PANICING.
    ///
    ///
    /// # Panics
    ///
    /// Panics if this Buffer contains no vector.
    /// [FIXME]: GET WORKING EVEN WITH NO CONTAINED VECTOR
    pub fn print(&mut self, every: usize, val_range: Option<(T, T)>, 
                idx_range_opt: Option<Range<usize>>, zeros: bool)
    {
        let idx_range = match idx_range_opt.clone() {
            Some(r) => r,
            None => 0..self.len(),
        };

        let vec = self.vec.as_mut().expect("Buffer::print()");

        unsafe { core::enqueue_read_buffer::<T, EventList>(
            &self.command_queue_obj_core, &self.obj_core, true, idx_range.start, 
            &mut vec[idx_range.clone()], None, None).unwrap() };
        util::print_slice(&vec[..], every, val_range, idx_range_opt, zeros);

    }

    /// Returns info about this buffer.
    pub fn mem_info(&self, info_kind: MemInfo) -> MemInfoResult {
        match core::get_mem_object_info(&self.obj_core, info_kind) {
            Ok(res) => res,
            Err(err) => MemInfoResult::Error(Box::new(err)),
        }        
    }

    pub fn fmt_mem_info(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Buffer Mem")
            .field("Type", &self.mem_info(MemInfo::Type))
            .field("Flags", &self.mem_info(MemInfo::Flags))
            .field("Size", &self.mem_info(MemInfo::Size))
            .field("HostPtr", &self.mem_info(MemInfo::HostPtr))
            .field("MapCount", &self.mem_info(MemInfo::MapCount))
            .field("ReferenceCount", &self.mem_info(MemInfo::ReferenceCount))
            .field("Context", &self.mem_info(MemInfo::Context))
            .field("AssociatedMemobject", &self.mem_info(MemInfo::AssociatedMemobject))
            .field("Offset", &self.mem_info(MemInfo::Offset))
            .finish()
    }
}

impl<T: OclNum> Deref for Buffer<T> {
    type Target = MemCore;

    fn deref(&self) -> &MemCore {
        &self.obj_core
    }
}

impl<T: OclNum> DerefMut for Buffer<T> {
    fn deref_mut(&mut self) -> &mut MemCore {
        &mut self.obj_core
    }
}

impl<T: OclNum> std::fmt::Display for Buffer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.fmt_mem_info(f)
    }
}


impl<T: OclNum> Index<usize> for Buffer<T> {
    type Output = T;
    /// # Panics
    /// Panics if this Buffer contains no vector.
    #[inline]
    fn index<'a>(&'a self, index: usize) -> &'a T {
        &self.vec.as_ref().expect("Buffer::index()")[..][index]
    }
}

impl<T: OclNum> IndexMut<usize> for Buffer<T> {
    /// # Panics
    /// Panics if this Buffer contains no vector.
    #[inline]
    fn index_mut<'a>(&'a mut self, index: usize) -> &'a mut T {
        &mut self.vec.as_mut().expect("Buffer::index_mut()")[..][index]
    }
}

impl<'b, T: OclNum> Index<&'b usize> for Buffer<T> {
    type Output = T;
    /// # Panics
    /// Panics if this Buffer contains no vector.
    #[inline]
    fn index<'a>(&'a self, index: &'b usize) -> &'a T {
        &self.vec.as_ref().expect("Buffer::index()")[..][*index]
    }
}

impl<'b, T: OclNum> IndexMut<&'b usize> for Buffer<T> {
    /// # Panics
    /// Panics if this Buffer contains no vector.
    #[inline]
    fn index_mut<'a>(&'a mut self, index: &'b usize) -> &'a mut T {
        &mut self.vec.as_mut().expect("Buffer::index_mut()")[..][*index]
    }
}

impl<T: OclNum> Index<Range<usize>> for Buffer<T> {
    type Output = [T];
    /// # Panics
    /// Panics if this Buffer contains no vector.
    #[inline]
    fn index<'a>(&'a self, range: Range<usize>) -> &'a [T] {
        &self.vec.as_ref().expect("Buffer::index()")[range]
    }
}

impl<T: OclNum> IndexMut<Range<usize>> for Buffer<T> {
    /// # Panics
    /// Panics if this Buffer contains no vector.
    #[inline]
    fn index_mut<'a>(&'a mut self, range: Range<usize>) -> &'a mut [T] {
        &mut self.vec.as_mut().expect("Buffer::index_mut()")[range]
    }
}

impl<T: OclNum> Index<RangeFull> for Buffer<T> {
    type Output = [T];
    /// # Panics
    /// Panics if this Buffer contains no vector.
    #[inline]
    fn index<'a>(&'a self, range: RangeFull) -> &'a [T] {
        &self.vec.as_ref().expect("Buffer::index()")[range]
    }
}

impl<T: OclNum> IndexMut<RangeFull> for Buffer<T> {
    /// # Panics
    /// Panics if this Buffer contains no vector.
    #[inline]
    fn index_mut<'a>(&'a mut self, range: RangeFull) -> &'a mut [T] {
        &mut self.vec.as_mut().expect("Buffer::index_mut()")[range]
    }
}

/// Returns a vector with length `size` containing random values in the (half-open)
/// range `[vals.0, vals.1)`.
pub fn scrambled_vec<T: OclNum>(size: usize, vals: (T, T)) -> Vec<T> {
    assert!(size > 0, "\nbuffer::shuffled_vec(): Vector size must be greater than zero.");
    assert!(vals.0 < vals.1, "\nbuffer::shuffled_vec(): Minimum value must be less than maximum.");
    let mut rng = rand::weak_rng();
    let range = RandRange::new(vals.0, vals.1);

    (0..size).map(|_| range.ind_sample(&mut rng)).take(size as usize).collect()
}

/// Returns a vector with length `size` which is first filled with each integer value
/// in the (inclusive) range `[vals.0, vals.1]`. If `size` is greater than the 
/// number of integers in the aforementioned range, the integers will repeat. After
/// being filled with `size` values, the vector is shuffled and the order of its
/// values is randomized.
pub fn shuffled_vec<T: OclNum>(size: usize, vals: (T, T)) -> Vec<T> {
    let mut vec: Vec<T> = Vec::with_capacity(size);
    assert!(size > 0, "\nbuffer::shuffled_vec(): Vector size must be greater than zero.");
    assert!(vals.0 < vals.1, "\nbuffer::shuffled_vec(): Minimum value must be less than maximum.");
    let min = vals.0.to_i64().expect("\nbuffer::shuffled_vec(), min");
    let max = vals.1.to_i64().expect("\nbuffer::shuffled_vec(), max") + 1;
    let mut range = (min..max).cycle();

    for _ in 0..size {
        vec.push(FromPrimitive::from_i64(range.next().expect("\nbuffer::shuffled_vec(), range")).expect("\nbuffer::shuffled_vec(), from_usize"));
    }

    shuffle_vec(&mut vec);
    vec
}


/// Shuffles the values in a vector using a single pass of Fisher-Yates with a
/// weak (not cryptographically secure) random number generator.
pub fn shuffle_vec<T: OclNum>(vec: &mut Vec<T>) {
    let len = vec.len();
    let mut rng = rand::weak_rng();
    let mut ridx: usize;
    let mut tmp: T;

    for i in 0..len {
        ridx = RandRange::new(i, len).ind_sample(&mut rng);
        tmp = vec[i];
        vec[i] = vec[ridx];
        vec[ridx] = tmp;
    }
}


// #[cfg(test)]
#[cfg(not(release))]
pub mod tests {
    use super::Buffer;
    use super::super::super::OclNum;
    use std::num::Zero;

    /// Test functions available to external crates.
    pub trait BufferTest<T> {
        fn read_idx_direct(&self, idx: usize) -> T;
    }

    impl<T: OclNum> BufferTest<T> for Buffer<T> {
        // Throw caution to the wind (this is potentially unsafe).
        fn read_idx_direct(&self, idx: usize) -> T {
            let mut buffer = vec![Zero::zero()];
            self.read(&mut buffer[0..1], idx).unwrap();
            buffer[0]
        }
    }
}



// impl<T> IntoIterator for Buffer<T> {
//     type Item = T;
//     type IntoIter = ::std::vec::IntoIter<T>;

//     fn into_iter(self) -> Self::IntoIter {
//      match self.vec {
//          VecOption::Some(vec) => vec.into_iter(),
//          VecOption::None => panic!("Buffer::into_iter(): Cannot iterate over a Buffer that
//              does not contain a built-in vector. Try creating your Buffer with ::with_vec()."),
//      }
//     }
// }


// impl<T: OclNum> Display for Buffer<T> {
//     fn fmt(&self, fmtr: &mut Formatter) -> FmtResult {
//      // self.print(1, None, None, true)
//      let mut tmp_vec = Vec::with_capacity(self.vec.len());
//      self.enqueue_read(&mut tmp_vec[..], 0);
//      fmt::fmt_vec(fmtr.buf, &tmp_vec[..], 1, None, None, true)
//  }
// }
