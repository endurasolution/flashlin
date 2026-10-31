pub const FLASHLIN_VERSION_CODE: &str = "FL-3.1.0-20261031";
pub const FLASHLIN_RELEASE_DATE: &str = "2026-10-31";
pub const FLASHLIN_LICENSE_VERSION: &str = "CC-BY-NC-4.0";
pub const FLASHLIN_LICENSE_TYPE: &str =
    "Creative Commons Attribution-NonCommercial 4.0 International";
pub const FLASHLIN_LICENSE_FILE_EXT: &str = ".flashlin";
pub const FLASHLIN_POWERED_BY: &str = "Endurasolution";

pub fn print_version() {
    println!("FlashLin {}", env!("CARGO_PKG_VERSION"));
    println!("Status: production release");
    println!(
        "Protocols: HTTP/1.1, HTTP/2{}",
        if cfg!(feature = "http3") {
            ", HTTP/3 (optional listener)"
        } else {
            ""
        }
    );
    println!(
        "TLS: {}; automatic certificates: {}",
        cfg!(feature = "tls"),
        cfg!(feature = "acme")
    );
    println!("Apps: supervised Node.js, Python and custom commands ([[apps]])");
    println!("Version Code: {}", FLASHLIN_VERSION_CODE);
    println!("Release Date: {}", FLASHLIN_RELEASE_DATE);
    println!("License Type: {}", FLASHLIN_LICENSE_TYPE);
    println!("License Version: {}", FLASHLIN_LICENSE_VERSION);
    println!("License View Format: *{}", FLASHLIN_LICENSE_FILE_EXT);
    println!("Powered By: {}", FLASHLIN_POWERED_BY);
}

pub fn print_license() {
    println!("{}", license_text());
}

pub fn license_text() -> String {
    format!(
        "FlashLin License View (*{})\n\
         Version Code: {}\n\
         Release Date: {}\n\
         FlashLin Version: {}\n\
         Powered By: {}\n\
         License Type: {}\n\
         License Version: {}\n\
         License URL: https://creativecommons.org/licenses/by-nc/4.0/\n\n\
         License\n\n\
         FlashLin is licensed under the Creative Commons Attribution-NonCommercial\n\
         4.0 International License (CC BY-NC 4.0).\n\n\
         You may share and adapt FlashLin for non-commercial use, provided that\n\
         appropriate credit is given to FlashLin and its developer/team, a link to\n\
         the license is provided, and changes are indicated where applicable.\n\n\
         Commercial use is not permitted without separate written permission from\n\
         the FlashLin developer/team.\n\n\
         Disclaimer\n\n\
         FlashLin is provided as-is and as-available, without warranties or\n\
         guarantees of any kind. The developer, team, contributors, and distributors\n\
         are not responsible or liable for any issue, damage, outage, security\n\
         incident, data loss, business loss, server problem, operating system\n\
         problem, configuration error, misuse, or any direct or indirect consequence\n\
         caused by installing, running, modifying, deploying, or using this software.\n\n\
         Users are responsible for testing, securing, configuring, backing up, and\n\
         operating their own systems before and after using FlashLin.\n\n\
         The authoritative license view is embedded in the FlashLin binary and can\n\
         be displayed with: flashlin --license\n",
        FLASHLIN_LICENSE_FILE_EXT,
        FLASHLIN_VERSION_CODE,
        FLASHLIN_RELEASE_DATE,
        env!("CARGO_PKG_VERSION"),
        FLASHLIN_POWERED_BY,
        FLASHLIN_LICENSE_TYPE,
        FLASHLIN_LICENSE_VERSION
    )
}
