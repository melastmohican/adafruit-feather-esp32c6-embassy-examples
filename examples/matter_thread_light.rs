//! # Matter Light Example over Thread for Adafruit Feather ESP32-C6
//!
//! Demonstrates a Matter On-Off Light device operating over Thread. The light can be
//! commissioned and controlled via a Matter controller such as Home Assistant.
//!
//! ## Hardware
//!
//! - **Board:** Adafruit Feather ESP32-C6
//! - **LED:** Connect an LED to GPIO 15 (e.g., Adafruit Feather's pin labeled "15" or any output pin), or monitor the pin state.
//!
//! ## Compilation Requirements (OpenThread)
//!
//! Building the embedded OpenThread stack requires C toolchain tools for compiling and binding the OpenThread C sources:
//!
//! 1. **Clang / LLVM:** Required by `bindgen` to generate the Rust bindings for the OpenThread C libraries.
//! 2. **RISC-V Cross Compiler:** A GCC toolchain for RISC-V targets (such as `riscv32-unknown-elf-gcc` or `riscv-none-elf-gcc`) must be available in your `PATH` so the `cc` build script can compile OpenThread for the ESP32-C6 target.
//!
//! ## Provisioning & Commissioning Fixes
//!
//! To successfully provision the device with Home Assistant / standard controllers, the following fixes were applied:
//!
//! 1. **Dummy Wireless Network Scan (`NoopWirelessNetCtl::scan`):**
//!    During concurrent BLE/Thread commissioning, the commissioner queries the device for a Wi-Fi network scan.
//!    Since this is a Thread-only example and Wi-Fi is stubbed out via `NoopWirelessNetCtl`, the scan function is patched to return `Ok(())` instead of `NotImplemented`. This prevents the controller from aborting the commissioning session in an infinite loop.
//! 2. **Logging Demotion in Interaction Model Status Responses:**
//!    The controller speculatively queries unsupported optional attributes/clusters (e.g., ICD Management or Ethernet Diagnostics).
//!    The code has been patched (`rs-matter` crate under `./rs-matter/rs-matter/src/dm/types/reply.rs`) to log these spec-compliant `UnsupportedCluster`/`UnsupportedAttribute` status responses as `debug!` rather than `error!`, cleaning up console spam.
//!
//! ## Run
//!
//! ```bash
//! cargo run --example matter_thread_light
//! ```
//!
//! > [!IMPORTANT]
//! > **Reprovisioning Requirement:** This example uses a RAM-only Key-Value store (`DummyKvBlobStore`) to hold its Matter credentials. Because these are not persisted to Flash, the device resets its pairing state on every reboot or reflash. You must remove the previous device from Home Assistant and pair it as a new device every time you re-run the example.

#![no_std]
#![no_main]
#![recursion_limit = "256"]

use core::pin::pin;
use embassy_executor::Spawner;
use esp_alloc::heap_allocator;
use esp_hal::timer::timg::TimerGroup;
use esp_metadata_generated::memory_range;
use panic_rtt_target as _;

use core::cell::RefCell;
use defmt::info;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::ram;
use rs_matter_embassy::matter::crypto::{Crypto, RngCore, default_crypto};
use rs_matter_embassy::matter::dm::clusters::app::on_off::test::TestOnOffDeviceLogic;
use rs_matter_embassy::matter::dm::clusters::app::on_off::{
    self, EffectVariantEnum, OnOffHooks, StartUpOnOffEnum,
};
use rs_matter_embassy::matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter_embassy::matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter_embassy::matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter_embassy::matter::dm::devices::test::{
    DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET,
};
use rs_matter_embassy::matter::dm::{
    Async, Cluster, Dataver, EmptyHandler, Endpoint, EpClMatcher, Node,
};
use rs_matter_embassy::matter::error::Error;
use rs_matter_embassy::matter::persist::DummyKvBlobStore;
use rs_matter_embassy::matter::tlv::Nullable;
use rs_matter_embassy::matter::utils::init::InitMaybeUninit;
use rs_matter_embassy::matter::utils::sync::blocking::Mutex;
use rs_matter_embassy::matter::{BasicCommData, clusters, devices};
use rs_matter_embassy::stack::rand::reseeding_csprng;
use rs_matter_embassy::wireless::esp::EspThreadDriver;
use rs_matter_embassy::wireless::{EmbassyThread, EmbassyThreadMatterStack};

use tinyrlibc as _;

extern crate alloc;

macro_rules! mk_static {
    ($t:ty) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        STATIC_CELL.uninit()
    }};
}

const BUMP_SIZE: usize = 25000;
const HEAP_SIZE: usize = 100 * 1024;

const RECLAIMED_RAM: usize =
    memory_range!("DRAM2_UNINIT").end - memory_range!("DRAM2_UNINIT").start;

esp_bootloader_esp_idf::esp_app_desc!();

struct LedOnOffDeviceLogic<'d> {
    led: Mutex<RefCell<Option<Output<'d>>>>,
    state: Mutex<RefCell<bool>>,
}

impl<'d> LedOnOffDeviceLogic<'d> {
    pub const fn new() -> Self {
        Self {
            led: Mutex::new(RefCell::new(None)),
            state: Mutex::new(RefCell::new(false)),
        }
    }

    pub fn set_led(&self, led: Output<'d>) {
        self.led.lock(|l| *l.borrow_mut() = Some(led));
    }
}

impl<'d> OnOffHooks for LedOnOffDeviceLogic<'d> {
    const CLUSTER: Cluster<'static> = TestOnOffDeviceLogic::CLUSTER;

    fn on_off(&self) -> bool {
        self.state.lock(|state| *state.borrow())
    }

    fn set_on_off(&self, on: bool) {
        self.state.lock(|state| *state.borrow_mut() = on);
        self.led.lock(|led| {
            if let Some(ref mut led) = *led.borrow_mut() {
                led.set_level(if on { Level::High } else { Level::Low });
            }
        });
    }

    fn start_up_on_off(&self) -> Nullable<StartUpOnOffEnum> {
        Nullable::none()
    }

    fn set_start_up_on_off(&self, _value: Nullable<StartUpOnOffEnum>) -> Result<(), Error> {
        Ok(())
    }

    async fn handle_off_with_effect(&self, _effect: EffectVariantEnum) {}
}

#[esp_rtos::main]
async fn main(_s: Spawner) {
    rtt_target::rtt_init_defmt!();

    info!("Starting matter_thread_light over Thread...");

    heap_allocator!(size: HEAP_SIZE - RECLAIMED_RAM);
    heap_allocator!(#[ram(reclaimed)] size: RECLAIMED_RAM);

    let peripherals = esp_hal::init(esp_hal::Config::default());

    let led = Output::new(peripherals.GPIO15, Level::Low, OutputConfig::default());

    let led_logic = LedOnOffDeviceLogic::new();
    led_logic.set_led(led);

    // Create the crypto provider, using the `esp-hal` TRNG/ADC1 as the source of randomness for a reseeding CSPRNG.
    let _trng_source = esp_hal::rng::TrngSource::new(peripherals.RNG, peripherals.ADC1);
    let crypto = default_crypto(
        reseeding_csprng(esp_hal::rng::Trng::try_new().unwrap(), 1000).unwrap(),
        DAC_PRIVKEY,
    );

    let mut weak_rand = crypto.weak_rand().unwrap();

    // in case there are left-overs from our previous registrations in Thread SRP
    let discriminator = (weak_rand.next_u32() & 0xfff) as u16;

    let mut ieee_eui64 = [0; 8];
    weak_rand.fill_bytes(&mut ieee_eui64);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT)
            .software_interrupt0,
    );

    // Allocate the Matter stack.
    let stack = mk_static!(EmbassyThreadMatterStack::<BUMP_SIZE, ()>).init_with(
        EmbassyThreadMatterStack::init(
            &TEST_BASIC_INFO,
            BasicCommData {
                password: TEST_DEV_COMM.password,
                discriminator,
            },
            &TEST_DEV_ATT,
        ),
    );

    // Our "light" on-off cluster.
    let on_off = on_off::OnOffHandler::new_standalone(
        Dataver::new_rand(&mut weak_rand),
        LIGHT_ENDPOINT_ID,
        &led_logic,
    );

    // Chain our endpoint clusters
    let handler = EmptyHandler
        // Our on-off cluster, on Endpoint 1
        .chain(
            EpClMatcher::new(
                Some(LIGHT_ENDPOINT_ID),
                Some(TestOnOffDeviceLogic::CLUSTER.id),
            ),
            on_off::HandlerAsyncAdaptor(&on_off),
        )
        // Each Endpoint needs a Descriptor cluster too
        .chain(
            EpClMatcher::new(Some(LIGHT_ENDPOINT_ID), Some(desc::DescHandler::CLUSTER.id)),
            Async(desc::DescHandler::new(Dataver::new_rand(&mut weak_rand)).adapt()),
        );

    // Create a KV BLOB store and load any previously saved state of `rs-matter`
    let mut kv = DummyKvBlobStore;
    stack.startup(&crypto, &mut kv).await.unwrap();

    // Wrap the KV BLOB store as a shared reference
    let kv = stack.create_shared_kv(kv).unwrap();

    // Run the Matter stack with our handler
    let matter = pin!(stack.run_coex(
        EmbassyThread::new(
            EspThreadDriver::new(peripherals.IEEE802154, peripherals.BT),
            crypto.rand().unwrap(),
            ieee_eui64,
            &kv,
            stack,
            true, // Use a random BLE address
        ),
        &crypto,
        (NODE, handler),
        &kv,
        (),
    ));

    // Run Matter
    matter.await.unwrap();
}

const TEST_BASIC_INFO: BasicInfoConfig = BasicInfoConfig {
    sai: Some(500),
    vendor_name: "melastmohican",
    product_name: "rs-matter esp32C6 light",
    ..TEST_DEV_DET
};

const LIGHT_ENDPOINT_ID: u16 = 1;

const NODE: Node = Node {
    endpoints: &[
        EmbassyThreadMatterStack::<0, ()>::root_endpoint(),
        Endpoint::new(
            LIGHT_ENDPOINT_ID,
            devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters!(desc::DescHandler::CLUSTER, TestOnOffDeviceLogic::CLUSTER),
        ),
    ],
};
