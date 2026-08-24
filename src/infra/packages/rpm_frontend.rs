#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RPMFrontends {
    DNF5,
    DNF,
    YUM,
    MICRODNF,
}

impl RPMFrontends {
    pub fn binary(self) -> &'static str {
        match self {
            RPMFrontends::DNF5 => "dnf5",
            RPMFrontends::DNF => "dnf",
            RPMFrontends::YUM => "yum",
            RPMFrontends::MICRODNF => "microdnf",
        }
    }

    pub fn detect() -> Option<Self> {
        [
            RPMFrontends::DNF5,
            RPMFrontends::DNF,
            RPMFrontends::YUM,
            RPMFrontends::MICRODNF,
        ]
        .into_iter()
        .find(|f| which::which(f.binary()).is_ok())
    }
}
