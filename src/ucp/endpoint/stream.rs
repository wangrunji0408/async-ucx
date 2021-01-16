use super::*;

impl Endpoint {
    pub async fn stream_send(&self, buf: &[u8]) -> usize {
        trace!("stream_send: endpoint={:?} len={}", self.handle, buf.len());
        unsafe extern "C" fn callback(request: *mut c_void, status: ucs_status_t) {
            trace!(
                "stream_send: complete. req={:?}, status={:?}",
                request,
                status
            );
            let request = &mut *(request as *mut Request);
            request.waker.wake();
        }
        let status = unsafe {
            ucp_stream_send_nb(
                self.handle,
                buf.as_ptr() as _,
                buf.len() as _,
                ucp_dt_make_contig(1),
                Some(callback),
                0,
            )
        };
        if status.is_null() {
            trace!("stream_send: complete");
        } else if UCS_PTR_IS_PTR(status) {
            RequestHandle {
                ptr: status,
                poll_fn: poll_normal,
            }
            .await;
        } else {
            panic!("failed to send stream: {:?}", UCS_PTR_RAW_STATUS(status));
        }
        buf.len()
    }

    pub async fn stream_recv(&self, buf: &mut [MaybeUninit<u8>]) -> usize {
        trace!("stream_recv: endpoint={:?} len={}", self.handle, buf.len());
        unsafe extern "C" fn callback(request: *mut c_void, status: ucs_status_t, length: u64) {
            trace!(
                "stream_recv: complete. req={:?}, status={:?}, len={}",
                request,
                status,
                length
            );
            let request = &mut *(request as *mut Request);
            request.waker.wake();
        }
        let mut length = MaybeUninit::uninit();
        let status = unsafe {
            ucp_stream_recv_nb(
                self.handle,
                buf.as_mut_ptr() as _,
                buf.len() as _,
                ucp_dt_make_contig(1),
                Some(callback),
                length.as_mut_ptr(),
                0,
            )
        };
        if status.is_null() {
            let length = unsafe { length.assume_init() } as usize;
            trace!("stream_recv: complete. len={}", length);
            length
        } else if UCS_PTR_IS_PTR(status) {
            RequestHandle {
                ptr: status,
                poll_fn: poll_stream,
            }
            .await
        } else {
            panic!("failed to recv stream: {:?}", UCS_PTR_RAW_STATUS(status));
        }
    }
}

unsafe fn poll_stream(ptr: ucs_status_ptr_t) -> Poll<usize> {
    let mut len = MaybeUninit::<usize>::uninit();
    let status = ucp_stream_recv_request_test(ptr as _, len.as_mut_ptr() as _);
    if status == ucs_status_t::UCS_INPROGRESS {
        Poll::Pending
    } else {
        Poll::Ready(len.assume_init())
    }
}
