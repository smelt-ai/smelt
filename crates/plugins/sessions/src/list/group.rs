//! 「筛选与分组」里选了按状态、按时间时，项目里的会话怎么分组。组是现算的：从按项目摆好的
//! 行里取出来重新分，不动项目里的手动顺序，也不存库。

use chrono::{DateTime, Local, TimeZone as _};
use smelt_plugin_api::model::agent_status::AgentStatus;

use crate::model::SidebarGrouping;

/// 按状态、时间分出来的一组。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Bucket {
    NeedsYou,
    Running,
    Idle,
    Today,
    Yesterday,
    /// 2 到 6 天前。
    Week,
    Earlier,
}

/// 按状态的组，从上到下。
const BY_STATUS: [Bucket; 3] = [Bucket::NeedsYou, Bucket::Running, Bucket::Idle];
/// 按时间的组，从上到下。
const BY_TIME: [Bucket; 4] = [
    Bucket::Today,
    Bucket::Yesterday,
    Bucket::Week,
    Bucket::Earlier,
];

impl Bucket {
    /// 组的标题。
    pub(super) fn label(self) -> &'static str {
        match self {
            Bucket::NeedsYou => "需要你",
            Bucket::Running => "运行中",
            Bucket::Idle => "空闲",
            Bucket::Today => "今天",
            Bucket::Yesterday => "昨天",
            Bucket::Week => "7 天内",
            Bucket::Earlier => "更早",
        }
    }

    fn of_status(status: AgentStatus) -> Self {
        match status {
            AgentStatus::NeedsYou => Bucket::NeedsYou,
            AgentStatus::Running => Bucket::Running,
            AgentStatus::Idle => Bucket::Idle,
        }
    }

    /// 最近动静（unix 秒）落在哪一天，按本机日历算。比 `now` 还晚（远程机器的时钟快）算
    /// 今天；不知道（0）、算不出来的算更早。
    fn of_time(at: u64, now: DateTime<Local>) -> Self {
        let Some(at) = i64::try_from(at)
            .ok()
            .and_then(|at| Local.timestamp_opt(at, 0).single())
        else {
            return Bucket::Earlier;
        };
        match (now.date_naive() - at.date_naive()).num_days() {
            days if days <= 0 => Bucket::Today,
            1 => Bucket::Yesterday,
            2..=6 => Bucket::Week,
            _ => Bucket::Earlier,
        }
    }
}

/// 把行分进组。`rows` 是每行的（状态，最近动静），按项目里从上到下的顺序。返回有行的组，从上
/// 到下，各带组里行的下标：按状态的组里保留原来的顺序，按时间的组里从新到旧（一样新的保留
/// 原来的顺序）。按项目不在这里分，返回空。
pub(super) fn buckets(
    grouping: SidebarGrouping,
    rows: &[(AgentStatus, u64)],
    now: DateTime<Local>,
) -> Vec<(Bucket, Vec<usize>)> {
    let mut ixs: Vec<usize> = (0..rows.len()).collect();
    let (order, of): (&[Bucket], Vec<Bucket>) = match grouping {
        SidebarGrouping::Project => return Vec::new(),
        SidebarGrouping::Status => (
            &BY_STATUS[..],
            rows.iter()
                .map(|(status, _)| Bucket::of_status(*status))
                .collect(),
        ),
        SidebarGrouping::LastUpdated => {
            // 稳定排序：一样新的保留原来的顺序。
            ixs.sort_by(|a, b| rows[*b].1.cmp(&rows[*a].1));
            (
                &BY_TIME[..],
                rows.iter()
                    .map(|(_, at)| Bucket::of_time(*at, now))
                    .collect(),
            )
        }
    };
    order
        .iter()
        .map(|bucket| {
            let members: Vec<usize> = ixs
                .iter()
                .copied()
                .filter(|ix| of[*ix] == *bucket)
                .collect();
            (*bucket, members)
        })
        .filter(|(_, members)| !members.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn flatten(groups: &[(Bucket, Vec<usize>)]) -> Vec<usize> {
        groups.iter().flat_map(|(_, ixs)| ixs.clone()).collect()
    }

    fn labels(groups: &[(Bucket, Vec<usize>)]) -> Vec<&'static str> {
        groups.iter().map(|(bucket, _)| bucket.label()).collect()
    }

    fn now() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 11, 17, 18, 0, 0)
            .single()
            .expect("本地时间")
    }

    /// `now` 那天往前 `days` 天的中午（unix 秒）。
    fn noon_days_ago(days: i64) -> u64 {
        let day = now().date_naive() - Duration::days(days);
        let noon = day.and_hms_opt(12, 0, 0).expect("中午");
        Local
            .from_local_datetime(&noon)
            .single()
            .expect("本地时间")
            .timestamp() as u64
    }

    #[test]
    fn status_grouping_order_matches_rendered_status_buckets() {
        use AgentStatus::*;
        let rows = [(Idle, 0), (Running, 0), (NeedsYou, 0), (Idle, 0)];
        let groups = buckets(SidebarGrouping::Status, &rows, now());
        assert_eq!(labels(&groups), ["需要你", "运行中", "空闲"]);
        assert_eq!(flatten(&groups), [2, 1, 0, 3]);
    }

    #[test]
    fn status_grouping_drops_empty_buckets() {
        let rows = [(AgentStatus::Idle, 5), (AgentStatus::Idle, 9)];
        let groups = buckets(SidebarGrouping::Status, &rows, now());
        assert_eq!(groups, [(Bucket::Idle, vec![0, 1])]);
        assert!(buckets(SidebarGrouping::Status, &[], now()).is_empty());
    }

    #[test]
    fn project_grouping_is_not_bucketed() {
        let rows = [(AgentStatus::NeedsYou, 1), (AgentStatus::Idle, 2)];
        assert!(buckets(SidebarGrouping::Project, &rows, now()).is_empty());
    }

    #[test]
    fn last_updated_grouping_uses_calendar_buckets_and_recent_order() {
        let rows = [0, 2, 1, 8].map(|days| (AgentStatus::Idle, noon_days_ago(days)));
        let groups = buckets(SidebarGrouping::LastUpdated, &rows, now());
        assert_eq!(labels(&groups), ["今天", "昨天", "7 天内", "更早"]);
        assert_eq!(flatten(&groups), [0, 2, 1, 3]);
    }

    #[test]
    fn last_updated_grouping_sorts_newest_first_inside_a_bucket() {
        let today = noon_days_ago(0);
        let rows = [
            (AgentStatus::Idle, today - 60),
            (AgentStatus::Idle, today),
            (AgentStatus::Idle, today - 60),
        ];
        let groups = buckets(SidebarGrouping::LastUpdated, &rows, now());
        assert_eq!(groups, [(Bucket::Today, vec![1, 0, 2])]);
    }

    #[test]
    fn time_buckets_handle_clock_skew_and_unknown_times() {
        let future = now().timestamp() as u64 + 3 * 86_400;
        assert_eq!(Bucket::of_time(future, now()), Bucket::Today);
        assert_eq!(Bucket::of_time(0, now()), Bucket::Earlier);
        assert_eq!(Bucket::of_time(u64::MAX, now()), Bucket::Earlier);
    }
}
