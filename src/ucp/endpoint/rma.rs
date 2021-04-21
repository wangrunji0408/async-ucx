use super::*;

#[derive(Debug)]
pub struct MemoryHandle {
    handle: ucp_mem_h,
    context: Rc<Context>,
}

impl MemoryHandle {
    pub fn register(context: &Rc<Context>, region: &mut [u8]) -> Self {
        let params = ucp_mem_map_params_t {
            field_mask: (ucp_mem_map_params_field::UCP_MEM_MAP_PARAM_FIELD_ADDRESS
                | ucp_mem_map_params_field::UCP_MEM_MAP_PARAM_FIELD_LENGTH)
                .0 as u64,
            address: region.as_ptr() as _,
            length: region.len() as _,
            flags: 0,
        };
        let mut handle = MaybeUninit::uninit();
        let status = unsafe { ucp_mem_map(context.handle, &params, handle.as_mut_ptr()) };
        assert_eq!(status, ucs_status_t::UCS_OK);
        MemoryHandle {
            handle: unsafe { handle.assume_init() },
            context: context.clone(),
        }
    }

    /// Packs into the buffer a remote access key (RKEY) object.
    pub fn pack(&self) -> RKeyBuffer {
        let mut buf = MaybeUninit::uninit();
        let mut len = MaybeUninit::uninit();
        let status = unsafe {
            ucp_rkey_pack(
                self.context.handle,
                self.handle,
                buf.as_mut_ptr(),
                len.as_mut_ptr(),
            )
        };
        assert_eq!(status, ucs_status_t::UCS_OK);
        RKeyBuffer {
            buf: unsafe { buf.assume_init() },
            len: unsafe { len.assume_init() },
        }
    }
}

impl Drop for MemoryHandle {
    fn drop(&mut self) {
        unsafe { ucp_mem_unmap(self.context.handle, self.handle) };
    }
}

#[derive(Debug)]
pub struct RKeyBuffer {
    buf: *mut c_void,
    len: u64,
}

impl AsRef<[u8]> for RKeyBuffer {
    fn as_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.buf as _, self.len as _) }
    }
}

impl Drop for RKeyBuffer {
    fn drop(&mut self) {
        unsafe { ucp_rkey_buffer_release(self.buf as _) }
    }
}

#[derive(Debug)]
pub struct RKey {
    handle: ucp_rkey_h,
}

unsafe impl Send for RKey {}
unsafe impl Sync for RKey {}

impl RKey {
    pub fn unpack(endpoint: &Endpoint, rkey_buffer: &[u8]) -> Self {
        let mut handle = MaybeUninit::uninit();
        let status = unsafe {
            ucp_ep_rkey_unpack(
                endpoint.handle,
                rkey_buffer.as_ptr() as _,
                handle.as_mut_ptr(),
            )
        };
        assert_eq!(status, ucs_status_t::UCS_OK);
        RKey {
            handle: unsafe { handle.assume_init() },
        }
    }
}

impl Drop for RKey {
    fn drop(&mut self) {
        unsafe { ucp_rkey_destroy(self.handle) }
    }
}

impl Endpoint {
    pub async fn put(&self, buf: &[u8], remote_addr: u64, rkey: &RKey) {
        trace!("put: endpoint={:?} len={}", self.handle, buf.len());
        unsafe extern "C" fn callback(request: *mut c_void, status: ucs_status_t) {
            trace!("put: complete. req={:?}, status={:?}", request, status);
            let request = &mut *(request as *mut Request);
            request.waker.wake();
        }
        let status = unsafe {
            ucp_put_nb(
                self.handle,
                buf.as_ptr() as _,
                buf.len() as _,
                remote_addr,
                rkey.handle,
                Some(callback),
            )
        };
        if status.is_null() {
            trace!("put: complete.");
        } else if UCS_PTR_IS_PTR(status) {
            RequestHandle {
                ptr: status,
                poll_fn: poll_normal,
            }
            .await;
        } else {
            panic!("failed to put: {:?}", UCS_PTR_RAW_STATUS(status));
        }
    }

    pub async fn get(&self, buf: &mut [u8], remote_addr: u64, rkey: &RKey) {
        trace!("get: endpoint={:?} len={}", self.handle, buf.len());
        unsafe extern "C" fn callback(request: *mut c_void, status: ucs_status_t) {
            trace!("get: complete. req={:?}, status={:?}", request, status);
            let request = &mut *(request as *mut Request);
            request.waker.wake();
        }
        let status = unsafe {
            ucp_get_nb(
                self.handle,
                buf.as_mut_ptr() as _,
                buf.len() as _,
                remote_addr,
                rkey.handle,
                Some(callback),
            )
        };
        if status.is_null() {
            trace!("get: complete.");
        } else if UCS_PTR_IS_PTR(status) {
            RequestHandle {
                ptr: status,
                poll_fn: poll_normal,
            }
            .await;
        } else {
            panic!("failed to get: {:?}", UCS_PTR_RAW_STATUS(status));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn put_get() {
        spawn_thread!(_put_get()).join().unwrap();
    }

    async fn _put_get() {
        let context1 = Context::new();
        let worker1 = context1.create_worker();
        let context2 = Context::new();
        let worker2 = context2.create_worker();
        tokio::task::spawn_local(worker1.clone().polling());
        tokio::task::spawn_local(worker2.clone().polling());

        // connect with each other
        let listener = worker1.create_listener("0.0.0.0:0".parse().unwrap());
        let listen_port = listener.socket_addr().port();
        let mut addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        addr.set_port(listen_port);
        let endpoint2 = worker2.create_endpoint(addr);
        let _endpoint1 = listener.accept().await;

        let mut buf1: Vec<u8> = vec![0; 0x1000];
        let mut buf2: Vec<u8> = (0..0x1000).map(|x| x as u8).collect();

        // register memory region
        let mem1 = MemoryHandle::register(&context1, &mut buf1);
        let rkey_buf = mem1.pack();
        let rkey2 = RKey::unpack(&endpoint2, rkey_buf.as_ref());

        // test put
        endpoint2
            .put(&buf2[..], buf1.as_mut_ptr() as u64, &rkey2)
            .await;
        assert_eq!(&buf1[..], &buf2[..]);

        // test get
        buf1.iter_mut().for_each(|x| *x = 0);
        endpoint2
            .get(&mut buf2[..], buf1.as_ptr() as u64, &rkey2)
            .await;
        assert_eq!(&buf1[..], &buf2[..]);
    }
}
