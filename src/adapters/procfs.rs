use std::{fs, io};

pub(super) fn read_process_identity(pid: u32) -> io::Result<(u32, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let started_at = parse_start_time(&stat)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process start time"))?;
    let uid = parse_real_uid(&status)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process UID"))?;
    Ok((uid, started_at))
}

fn parse_start_time(stat: &str) -> Option<u64> {
    let command_end = stat.rfind(')')?;
    let mut fields_after_command = stat.get(command_end + 1..)?.split_whitespace();
    fields_after_command.nth(19)?.parse().ok()
}

fn parse_real_uid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_real_uid, parse_start_time};

    #[test]
    fn parses_start_time_after_a_command_with_spaces() {
        let stat = "42 (resource guard) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 20";

        assert_eq!(parse_start_time(stat), Some(98_765));
    }

    #[test]
    fn parses_start_time_after_a_command_containing_parentheses() {
        let stat = "42 (worker (busy)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 20";

        assert_eq!(parse_start_time(stat), Some(12_345));
    }

    #[test]
    fn rejects_an_incomplete_stat_record() {
        assert_eq!(parse_start_time("42 (worker) S 1 2 3"), None);
    }

    #[test]
    fn parses_real_uid_from_status() {
        let status = "Name:\tworker\nUid:\t1000\t1001\t1002\t1003\nGid:\t1000\t1000\t1000\t1000\n";

        assert_eq!(parse_real_uid(status), Some(1_000));
    }
}
