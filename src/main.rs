use std::thread;
use std::time::Duration;

use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::sys::EspError;

fn main() -> Result<(), EspError> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("Starting LED Flashing Demo!");

    // Take the peripherals
    let peripherals = Peripherals::take().unwrap();

    // Create LED drivers for GPIO pins 2, 4, and 5
    let mut led_red = PinDriver::output(peripherals.pins.gpio2)?;
    let mut led_green = PinDriver::output(peripherals.pins.gpio4)?;
    let mut led_blue = PinDriver::output(peripherals.pins.gpio5)?;

    log::info!("LEDs initialized on GPIO2 (Red), GPIO4 (Green), GPIO5 (Blue)");

    // LED patterns counter
    let mut pattern = 0;

    loop {
        match pattern {
            // Pattern 0: Sequential flashing (Red -> Green -> Blue)
            0 => {
                log::info!("Pattern 1: Sequential flashing");
                for _ in 0..3 {
                    // Red LED
                    led_red.set_high()?;
                    thread::sleep(Duration::from_millis(300));
                    led_red.set_low()?;
                    thread::sleep(Duration::from_millis(100));

                    // Green LED
                    led_green.set_high()?;
                    thread::sleep(Duration::from_millis(300));
                    led_green.set_low()?;
                    thread::sleep(Duration::from_millis(100));

                    // Blue LED
                    led_blue.set_high()?;
                    thread::sleep(Duration::from_millis(300));
                    led_blue.set_low()?;
                    thread::sleep(Duration::from_millis(100));
                }
            }

            // Pattern 1: All LEDs flashing together
            1 => {
                log::info!("Pattern 2: All LEDs flashing together");
                for _ in 0..5 {
                    led_red.set_high()?;
                    led_green.set_high()?;
                    led_blue.set_high()?;
                    thread::sleep(Duration::from_millis(500));

                    led_red.set_low()?;
                    led_green.set_low()?;
                    led_blue.set_low()?;
                    thread::sleep(Duration::from_millis(500));
                }
            }

            // Pattern 2: Alternating pairs
            2 => {
                log::info!("Pattern 3: Alternating pairs");
                for _ in 0..4 {
                    // Red and Blue on, Green off
                    led_red.set_high()?;
                    led_green.set_low()?;
                    led_blue.set_high()?;
                    thread::sleep(Duration::from_millis(400));

                    // Green on, Red and Blue off
                    led_red.set_low()?;
                    led_green.set_high()?;
                    led_blue.set_low()?;
                    thread::sleep(Duration::from_millis(400));
                }
            }

            // Pattern 3: Fast blink individual LEDs
            _ => {
                log::info!("Pattern 4: Fast individual blinks");
                // Fast red blinks
                for _ in 0..6 {
                    led_red.set_high()?;
                    thread::sleep(Duration::from_millis(100));
                    led_red.set_low()?;
                    thread::sleep(Duration::from_millis(100));
                }

                // Fast green blinks
                for _ in 0..6 {
                    led_green.set_high()?;
                    thread::sleep(Duration::from_millis(100));
                    led_green.set_low()?;
                    thread::sleep(Duration::from_millis(100));
                }

                // Fast blue blinks
                for _ in 0..6 {
                    led_blue.set_high()?;
                    thread::sleep(Duration::from_millis(100));
                    led_blue.set_low()?;
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }

        // Move to next pattern, cycle back to 0 after pattern 3
        pattern = (pattern + 1) % 4;

        // Brief pause between patterns
        thread::sleep(Duration::from_millis(1000));
    }
}
