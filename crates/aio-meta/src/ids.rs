#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Movie,
    Series,
}

impl Kind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "movie" => Some(Self::Movie),
            "series" => Some(Self::Series),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id {
    Imdb(String),
    Tmdb(u64),
    Tvdb(u64),
    Tvmaze(u64),
}

impl Id {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.len() > 200 || raw.chars().any(char::is_control) {
            return None;
        }
        if let Some(rest) = raw.strip_prefix("tt") {
            if (5..=12).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_digit()) {
                return Some(Self::Imdb(format!("tt{rest}")));
            }
            return None;
        }
        let (prefix, number) = raw.split_once(':')?;
        if number.is_empty() || number.len() > 20 || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let number = number.parse::<u64>().ok()?;
        match prefix {
            "tmdb" => Some(Self::Tmdb(number)),
            "tvdb" => Some(Self::Tvdb(number)),
            "tvmaze" => Some(Self::Tvmaze(number)),
            _ => None,
        }
    }

    pub fn base(&self) -> String {
        match self {
            Self::Imdb(value) => value.clone(),
            Self::Tmdb(value) => format!("tmdb:{value}"),
            Self::Tvdb(value) => format!("tvdb:{value}"),
            Self::Tvmaze(value) => format!("tvmaze:{value}"),
        }
    }
}

pub fn canonical(imdb: Option<&str>, fallback: &str) -> String {
    let Some(imdb) = imdb.map(str::trim).filter(|value| !value.is_empty()) else {
        return fallback.to_owned();
    };
    if imdb.starts_with("tt")
        && (5..=12).contains(&imdb[2..].len())
        && imdb[2..].bytes().all(|b| b.is_ascii_digit())
    {
        imdb.to_owned()
    } else {
        fallback.to_owned()
    }
}
