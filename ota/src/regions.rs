//! The update servers, and the carrier and language each region is asked with.
//!
//! The carriers (NV ids) are those OPPO's own phones carry in
//! `ro.build.oplus_nv_id`; the server picks the build by them. The list
//! follows NeFeroN's LinkResolverDownloader, checked against the servers.

const HOST_CN: &str = "component-ota-cn.allawntech.com";
const HOST_EU: &str = "component-ota-eu.allawnos.com";
const HOST_IN: &str = "component-ota-in.allawnos.com";
const HOST_SG: &str = "component-ota-sg.allawnos.com";

struct Info {
    code: &'static str,
    name: &'static str,
    carrier: &'static str,
    language: &'static str,
    host: &'static str,
    /// Added to the model in the `model` header (`CPH2747EEA`, `RMX3709TR`):
    /// the region's newest builds answer only to it.
    suffix: &'static str,
}

const fn info(
    code: &'static str,
    name: &'static str,
    carrier: &'static str,
    language: &'static str,
    host: &'static str,
    suffix: &'static str,
) -> Info {
    Info {
        code,
        name,
        carrier,
        language,
        host,
        suffix,
    }
}

// Asked in this order; a build found in several regions lists them so.
const TABLE: &[Info] = &[
    info("GL", "Global", "10100111", "en-US", HOST_SG, ""),
    info("EU", "Europe", "01000100", "en-GB", HOST_EU, "EEA"),
    info("IN", "India", "00011011", "en-IN", HOST_IN, "IN"),
    info("RU", "Russia", "00110111", "ru-RU", HOST_SG, "RU"),
    info(
        "MEA",
        "Middle East and Africa",
        "10100110",
        "en-US",
        HOST_SG,
        "MEA",
    ),
    info("TR", "Turkey", "01010001", "tr-TR", HOST_SG, "TR"),
    info("SA", "Saudi Arabia", "10000011", "ar-SA", HOST_SG, "SA"),
    info("SG", "Singapore", "01011010", "en-SG", HOST_SG, "SG"),
    info("TH", "Thailand", "00111001", "th-TH", HOST_SG, "TH"),
    info("MY", "Malaysia", "00111000", "ms-MY", HOST_SG, "MY"),
    info("ID", "Indonesia", "00110011", "id-ID", HOST_SG, "ID"),
    info("PH", "Philippines", "00111110", "en-PH", HOST_SG, "PH"),
    info("VN", "Vietnam", "00111100", "vi-VN", HOST_SG, "VN"),
    info("TW", "Taiwan", "00011010", "zh-TW", HOST_SG, "TW"),
    info("OCA", "Oceania", "10100101", "en-AU", HOST_SG, "OCA"),
    info(
        "LATAM",
        "Latin America",
        "10011010",
        "es-MX",
        HOST_SG,
        "LATAM",
    ),
    info("BR", "Brazil", "10011110", "pt-BR", HOST_SG, "BR"),
    info("GB", "United Kingdom", "10001010", "en-GB", HOST_SG, "GB"),
    info("CN", "China", "10010111", "zh-CN", HOST_CN, ""),
];

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Region(u8);

impl std::fmt::Debug for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl Region {
    pub const GL: Region = Region(0);
    pub const EU: Region = Region(1);
    pub const IN: Region = Region(2);
    pub const CN: Region = Region(18);

    pub fn all() -> impl Iterator<Item = Region> {
        (0..TABLE.len() as u8).map(Region)
    }

    fn info(self) -> &'static Info {
        &TABLE[self.0 as usize]
    }

    pub fn code(self) -> &'static str {
        self.info().code
    }

    /// In English, e.g. "Middle East and Africa".
    pub fn name(self) -> &'static str {
        self.info().name
    }

    pub fn parse(s: &str) -> Option<Region> {
        let s = s.trim();
        // "GLO" as LinkResolverDownloader writes it.
        let s = if s.eq_ignore_ascii_case("GLO") {
            "GL"
        } else {
            s
        };
        Region::all().find(|r| r.code().eq_ignore_ascii_case(s))
    }

    /// The region a phone belongs to, from its `ro.build.oplus_nv_id`.
    pub fn from_carrier(nv_id: &str) -> Option<Region> {
        Region::all().find(|r| r.carrier() == nv_id.trim())
    }

    pub fn host(self) -> &'static str {
        self.info().host
    }

    /// Hosts that answer this region: China only on its own, the rest on
    /// any of the international ones.
    pub fn hosts(self) -> &'static [&'static str] {
        if self.host() == HOST_CN {
            &[HOST_CN]
        } else {
            &[HOST_SG, HOST_EU, HOST_IN]
        }
    }

    /// The NV carrier id the server picks the build by.
    pub fn carrier(self) -> &'static str {
        self.info().carrier
    }

    /// The model as the region's phones name themselves (`CPH2747EEA`,
    /// `RMX3709TR`); `otaVersion` keeps the plain code.
    pub fn model(self, code: &str) -> String {
        let suffix = self.info().suffix;
        if code.ends_with(suffix) {
            code.to_string()
        } else {
            format!("{code}{suffix}")
        }
    }

    pub fn language(self) -> &'static str {
        self.info().language
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codes_in_any_case() {
        assert_eq!(Region::parse("gl"), Some(Region::GL));
        assert_eq!(Region::parse("GLO"), Some(Region::GL));
        assert_eq!(Region::parse(" EU "), Some(Region::EU));
        assert_eq!(Region::parse("mea").map(Region::code), Some("MEA"));
        assert_eq!(Region::parse("XX"), None);
        assert_eq!(Region::EU.model("CPH2747"), "CPH2747EEA");
        assert_eq!(Region::EU.model("CPH2747EEA"), "CPH2747EEA");
        assert_eq!(Region::IN.model("CPH2745"), "CPH2745IN");
        assert_eq!(Region::GL.model("CPH2747"), "CPH2747");
        assert_eq!(Region::from_carrier("10100111"), Some(Region::GL));
        assert_eq!(Region::from_carrier("00000000"), None);
        assert_eq!(Region::CN.code(), "CN");
        for r in Region::all() {
            assert_eq!(Region::parse(r.code()), Some(r));
        }
    }

    #[test]
    fn carriers_are_unique_binary_bytes() {
        let mut seen = std::collections::HashSet::new();
        for r in Region::all() {
            let c = r.carrier();
            assert!(
                c.len() == 8 && c.chars().all(|b| b == '0' || b == '1'),
                "{c}"
            );
            assert!(seen.insert(c), "{c} twice");
        }
    }
}
