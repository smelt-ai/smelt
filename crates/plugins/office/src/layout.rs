//! 自动隔间的像素坐标；绘制和走路共用门口与通道位置。

pub(crate) const WIDTH: f32 = 192.;
pub(crate) const HEIGHT: f32 = 176.;
pub(crate) const DOOR_LEFT: i32 = 32;
pub(crate) const DOOR_RIGHT: i32 = 60;
pub(crate) const FRONT_Y: i32 = 54;
pub(crate) const WALL_DEPTH: i32 = 6;
pub(crate) const AISLE_Y: f32 = 65.;
pub(crate) const WORK: (f32, f32) = (40., 35.);
pub(crate) const WAIT: (f32, f32) = (46., 41.);
pub(crate) const REST: (f32, f32) = (12., 59.);
pub(crate) const ENTRANCE: (f32, f32) = (76., AISLE_Y);

pub(crate) fn room_width(workers: usize) -> f32 {
    // Button 的横向内边距为 14pt；工位之间 8pt，房间两侧各 12pt。
    workers.clamp(1, 4) as f32 * (WIDTH + 28. + 8.) + 16.
}

pub(crate) fn columns(width: f32) -> usize {
    (((width - 16.).max(0.) / (WIDTH + 36.)) as usize).clamp(1, 4)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Header(usize),
    Workers {
        room: usize,
        workers: std::ops::Range<usize>,
        first: bool,
        last: bool,
    },
}

pub(crate) fn rows(counts: impl IntoIterator<Item = usize>, columns: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    for (room, count) in counts.into_iter().enumerate() {
        if count == 0 {
            continue;
        }
        rows.push(Row::Header(room));
        for first in (0..count).step_by(columns.max(1)) {
            let end = (first + columns.max(1)).min(count);
            rows.push(Row::Workers {
                room,
                workers: first..end,
                first: first == 0,
                last: end == count,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooms_fit_their_workers_and_stop_growing_after_four_columns() {
        assert_eq!(room_width(0), room_width(1));
        assert_eq!(room_width(1), 244.);
        assert_eq!(room_width(2), 472.);
        assert_eq!(room_width(4), 928.);
        assert_eq!(room_width(50), room_width(4));
    }

    #[test]
    fn responsive_rows_cover_every_worker_once_without_empty_rooms() {
        assert_eq!(columns(244.), 1);
        assert_eq!(columns(472.), 2);
        assert_eq!(columns(928.), 4);
        assert_eq!(columns(100.), 1);
        for cols in 1..=4 {
            let rows = rows([100, 0, 3], cols);
            let mut seen = Vec::new();
            for row in rows {
                if let Row::Workers { room, workers, .. } = row {
                    assert!(workers.len() <= cols);
                    seen.extend(workers.map(|worker| (room, worker)));
                }
            }
            let expected: Vec<_> = (0..100)
                .map(|i| (0, i))
                .chain((0..3).map(|i| (2, i)))
                .collect();
            assert_eq!(seen, expected);
        }
        assert!(rows([], 4).is_empty());
        assert_eq!(rows([1], 0).len(), 2);
    }
}
