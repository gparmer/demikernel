use hashbrown::HashMap;
use dpdk_rs::{
    rte_eth_dev,
    rte_eth_devices,
    rte_mbuf,
    rte_mempool,
};
use catnip::{
    protocols::{
        arp,
        ethernet2::MacAddress,
        tcp,
    },
    runtime::{
        PacketBuf,
        Runtime,
    },
    scheduler::{
        Operation,
        Scheduler,
        SchedulerHandle,
    },
    sync::{
        Bytes,
        BytesMut,
    },
    timer::{
        Timer,
        TimerPtr,
        WaitFuture,
    },
};
use dpdk_rs::{
    rte_pktmbuf_free,
    rte_pktmbuf_alloc,
    rte_eth_tx_burst,
    rte_eth_rx_burst,
};
use futures::FutureExt;
use rand::{
    distributions::{
        Distribution,
        Standard,
    },
    rngs::SmallRng,
    Rng,
    SeedableRng,
};
use std::{
    cell::RefCell,
    future::Future,
    mem,
    mem::MaybeUninit,
    net::Ipv4Addr,
    ptr,
    rc::Rc,
    slice,
    time::{
        Duration,
        Instant,
    },
};

const MAX_QUEUE_DEPTH: usize = 16;

#[derive(Clone)]
pub struct TimerRc(Rc<Timer<TimerRc>>);

impl TimerPtr for TimerRc {
    fn timer(&self) -> &Timer<Self> {
        &*self.0
    }
}

#[derive(Clone)]
pub struct DPDKRuntime {
    inner: Rc<RefCell<Inner>>,
    scheduler: Scheduler<Operation<Self>>,
}

impl DPDKRuntime {
    pub fn new(
        link_addr: MacAddress,
        ipv4_addr: Ipv4Addr,
        dpdk_port_id: u16,
        dpdk_mempool: *mut rte_mempool,
        arp_table: HashMap<MacAddress, Ipv4Addr>,
        disable_arp: bool,
    ) -> Self {
        let mut rng = rand::thread_rng();
        let rng = SmallRng::from_rng(&mut rng).expect("Failed to initialize RNG");
        let now = Instant::now();

        let mut buffered: MaybeUninit<[Bytes; MAX_QUEUE_DEPTH]> = MaybeUninit::uninit();
        for i in 0..MAX_QUEUE_DEPTH {
            unsafe {
                (buffered.as_mut_ptr() as *mut Bytes)
                    .offset(i as isize)
                    .write(Bytes::empty())
            };
        }
        let mut arp_options = arp::Options::default();
        arp_options.initial_values = arp_table;
        arp_options.disable_arp = disable_arp;
        let inner = Inner {
            timer: TimerRc(Rc::new(Timer::new(now))),
            link_addr,
            ipv4_addr,
            rng,
            arp_options,
            tcp_options: tcp::Options::default(),

            dpdk_port_id,
            dpdk_mempool,

            num_buffered: 0,
            buffered: unsafe { buffered.assume_init() },

            pool: (0..POOL_SIZE).map(|_| unsafe { Rc::new_zeroed_slice(ALLOC_SIZE).assume_init() }).collect(),
        };
        Self {
            inner: Rc::new(RefCell::new(inner)),
            scheduler: Scheduler::new(),
        }
    }
}

const ALLOC_SIZE: usize = 12288;
const POOL_SIZE: usize = 1024;

struct Inner {
    timer: TimerRc,
    link_addr: MacAddress,
    ipv4_addr: Ipv4Addr,
    rng: SmallRng,
    arp_options: arp::Options,
    tcp_options: tcp::Options,

    dpdk_port_id: u16,
    dpdk_mempool: *mut rte_mempool,

    num_buffered: usize,
    buffered: [Bytes; MAX_QUEUE_DEPTH],

    pool: Vec<Rc<[u8]>>,
}

#[inline(never)]
fn noop_span_log() {
    let _s = tracy_client::static_span!();
}

impl Runtime for DPDKRuntime {
    type WaitFuture = WaitFuture<TimerRc>;

    fn transmit(&self, buf: impl PacketBuf) {
        let pool = { self.inner.borrow().dpdk_mempool };
        let dpdk_port_id = { self.inner.borrow().dpdk_port_id };
        let mut pkt = unsafe { rte_pktmbuf_alloc(pool) };
        assert!(!pkt.is_null());

        let size = buf.compute_size();

        let rte_pktmbuf_headroom = 128;
        let buf_len = unsafe { (*pkt).buf_len } - rte_pktmbuf_headroom;
        assert!(buf_len as usize >= size);

        {
            let _t = tracy_client::static_span!("serialize");
            let out_ptr = unsafe { ((*pkt).buf_addr as *mut u8).offset((*pkt).data_off as isize) };
            let out_slice = unsafe { slice::from_raw_parts_mut(out_ptr, buf_len as usize) };
            buf.serialize(&mut out_slice[..size]);
        }
        let num_sent = unsafe {
            (*pkt).data_len = size as u16;
            (*pkt).pkt_len = size as u32;
            (*pkt).nb_segs = 1;
            (*pkt).next = ptr::null_mut();

            rte_eth_tx_burst(dpdk_port_id, 0, &mut pkt as *mut _, 1)
        };
        assert_eq!(num_sent, 1);
    }

    fn receive(&self) -> Option<Bytes> {
        // noop_span_log();
        let mut inner = self.inner.borrow_mut();
        loop {
            if inner.num_buffered > 0 {
                inner.num_buffered -= 1;
                let ix = inner.num_buffered;
                return Some(mem::replace(&mut inner.buffered[ix], Bytes::empty()));
            }

            let dpdk_port = inner.dpdk_port_id;
            let mut packets: [*mut rte_mbuf; MAX_QUEUE_DEPTH] = unsafe { mem::zeroed() };

            // rte_eth_rx_burst is declared `inline` in the header.
            let nb_rx = unsafe {
                rte_eth_rx_burst(
                    dpdk_port,
                    0,
                    packets.as_mut_ptr(),
                    MAX_QUEUE_DEPTH as u16,
                )
            };
            assert!(nb_rx as usize <= MAX_QUEUE_DEPTH);
            if nb_rx == 0 {
                return None;
            }
            // let dev = unsafe { rte_eth_devices[dpdk_port as usize] };
            // let rx_burst = dev.rx_pkt_burst.expect("Missing RX burst function");
            // // This only supports queue_id 0.
            // let nb_rx = unsafe { (rx_burst)(*(*dev.data).rx_queues, todo!(), MAX_QUEUE_DEPTH as u16) };

            for &packet in &packets[..nb_rx as usize] {
                // println!("Receiving packet at timestamp {}", unsafe { (*packet).timestamp });
                // auto * const p = rte_pktmbuf_mtod(packet, uint8_t *);
                let p = unsafe {
                    ((*packet).buf_addr as *const u8).offset((*packet).data_off as isize)
                };

                let data = unsafe { slice::from_raw_parts(p, (*packet).data_len as usize) };

                let buf = inner.pool.pop().unwrap_or_else(|| unsafe { Rc::new_zeroed_slice(ALLOC_SIZE).assume_init() });
                let mut buf = BytesMut::from_buf(buf);

                let buf = {
                    // let _t = tracy_client::static_span!("copy");
                    buf[..data.len()].copy_from_slice(data);
                    let (buf, _) = buf.freeze().split(data.len());
                    buf
                };
                let ix = inner.num_buffered;
                inner.buffered[ix] = buf; 
                inner.num_buffered += 1;

                unsafe { rte_pktmbuf_free(packet as *const _ as *mut _) };
            }
        }
    }

    fn local_link_addr(&self) -> MacAddress {
        self.inner.borrow().link_addr.clone()
    }

    fn local_ipv4_addr(&self) -> Ipv4Addr {
        self.inner.borrow().ipv4_addr.clone()
    }

    fn tcp_options(&self) -> tcp::Options {
        self.inner.borrow().tcp_options.clone()
    }

    fn arp_options(&self) -> arp::Options {
        self.inner.borrow().arp_options.clone()
    }

    fn advance_clock(&self, now: Instant) {
        self.inner.borrow_mut().timer.0.advance_clock(now);
    }

    fn wait(&self, duration: Duration) -> Self::WaitFuture {
        let self_ = self.inner.borrow_mut();
        let now = self_.timer.0.now();
        self_
            .timer
            .0
            .wait_until(self_.timer.clone(), now + duration)
    }

    fn wait_until(&self, when: Instant) -> Self::WaitFuture {
        let self_ = self.inner.borrow_mut();
        self_.timer.0.wait_until(self_.timer.clone(), when)
    }

    fn now(&self) -> Instant {
        self.inner.borrow().timer.0.now()
    }

    fn rng_gen<T>(&self) -> T
    where
        Standard: Distribution<T>,
    {
        let mut self_ = self.inner.borrow_mut();
        self_.rng.gen()
    }

    fn spawn<F: Future<Output = ()> + 'static>(&self, future: F) -> SchedulerHandle {
        self.scheduler
            .insert(Operation::Background(future.boxed_local()))
    }

    fn scheduler(&self) -> &Scheduler<Operation<Self>> {
        &self.scheduler
    }

    fn donate_buffer(&self, buf: Rc<[u8]>) {
        if buf.len() == ALLOC_SIZE {
            self.inner.borrow_mut().pool.push(buf);
        }
    }
}
