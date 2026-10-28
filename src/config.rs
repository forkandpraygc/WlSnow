use clap::Parser;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub flake_count: usize,
    pub speed: f32,
    pub size: f32,
    pub drift: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            flake_count: 200,
            speed: 60.0,
            size: 3.0,
            drift: 10.0,
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(name = "wlsnow", version)]
pub struct Cli {
    #[arg(long)]
    pub count: Option<usize>,
    #[arg(long)]
    pub speed: Option<f32>,
    #[arg(long)]
    pub size: Option<f32>,
    #[arg(long)]
    pub drift: Option<f32>,
}

impl Config {
    pub fn from_cli(cli: &Cli) -> Self {
        let defaults = Config::default();
        Config {
            flake_count: cli.count.unwrap_or(defaults.flake_count),
            speed: cli.speed.unwrap_or(defaults.speed),
            size: cli.size.unwrap_or(defaults.size),
            drift: cli.drift.unwrap_or(defaults.drift),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.flake_count, 200);
        assert_eq!(c.speed, 60.0);
        assert_eq!(c.size, 3.0);
        assert_eq!(c.drift, 10.0);
    }

    #[test]
    fn empty_cli_uses_defaults() {
        let cli = Cli {
            count: None,
            speed: None,
            size: None,
            drift: None,
        };
        assert_eq!(Config::from_cli(&cli), Config::default());
    }

    #[test]
    fn cli_overrides_defaults() {
        let cli = Cli {
            count: Some(50),
            speed: Some(120.0),
            size: Some(5.0),
            drift: None,
        };
        let c = Config::from_cli(&cli);
        assert_eq!(c.flake_count, 50);
        assert_eq!(c.speed, 120.0);
        assert_eq!(c.size, 5.0);
        assert_eq!(c.drift, 10.0);
    }
}
