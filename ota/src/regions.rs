//! The update servers, and the carrier and language each region is asked with.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    Cn,
    Eu,
    In,
    Sg,
    Ru,
    Tr,
    Th,
    /// Global builds ("EX"): OnePlus phones sold outside China answer here.
    Gl,
}

const HOST_CN: &str = "component-ota-cn.allawntech.com";
const HOST_EU: &str = "component-ota-eu.allawnos.com";
const HOST_IN: &str = "component-ota-in.allawnos.com";
const HOST_SG: &str = "component-ota-sg.allawnos.com";

impl Region {
    pub const ALL: [Region; 8] = [
        Region::Gl,
        Region::Eu,
        Region::In,
        Region::Sg,
        Region::Ru,
        Region::Tr,
        Region::Th,
        Region::Cn,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Region::Cn => "CN",
            Region::Eu => "EU",
            Region::In => "IN",
            Region::Sg => "SG",
            Region::Ru => "RU",
            Region::Tr => "TR",
            Region::Th => "TH",
            Region::Gl => "GL",
        }
    }

    pub fn parse(s: &str) -> Option<Region> {
        Region::ALL
            .into_iter()
            .find(|r| r.code().eq_ignore_ascii_case(s.trim()))
    }

    pub fn host(self) -> &'static str {
        match self {
            Region::Cn => HOST_CN,
            Region::Eu => HOST_EU,
            Region::In => HOST_IN,
            Region::Sg | Region::Ru | Region::Tr | Region::Th | Region::Gl => HOST_SG,
        }
    }

    /// The NV carrier id the server picks the build by.
    pub fn carrier(self) -> &'static str {
        match self {
            Region::Cn => "10010111",
            Region::Eu => "01000100",
            Region::In => "00011011",
            Region::Sg => "01011010",
            Region::Ru => "00110111",
            Region::Tr => "01010001",
            Region::Th => "00111001",
            Region::Gl => "10100111",
        }
    }

    pub fn language(self) -> &'static str {
        match self {
            Region::Cn => "zh-CN",
            Region::Eu => "en-GB",
            Region::In => "en-IN",
            Region::Sg => "en-SG",
            Region::Ru => "ru-RU",
            Region::Tr => "tr-TR",
            Region::Th => "th-TH",
            Region::Gl => "en-US",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codes_in_any_case() {
        assert_eq!(Region::parse("gl"), Some(Region::Gl));
        assert_eq!(Region::parse(" EU "), Some(Region::Eu));
        assert_eq!(Region::parse("XX"), None);
        for r in Region::ALL {
            assert_eq!(Region::parse(r.code()), Some(r));
        }
    }
}
