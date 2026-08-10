//! 弹幕轨道布局（us-danmaku 算法）：滚动/顶部/底部弹幕的位置与时间计算。

use super::{BurnConfig, DanmakuItem, PositionedDanmaku, MAX_DELAY, PLAY_RES_X, PLAY_RES_Y, SPACE};

pub(super) fn set_position(
    danmaku_list: &[DanmakuItem],
    config: &BurnConfig,
) -> Vec<PositionedDanmaku> {
    let mut normal = NormalDanmaku::new(config);
    let mut side = SideDanmaku::new(config);

    let mut positioned = Vec::new();
    let mut sorted: Vec<DanmakuItem> = danmaku_list.to_vec();
    sorted.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for line in sorted {
        if line.text.is_empty() {
            continue;
        }
        // 应用字号缩放：scale=1.0 时保持原大小，>1 放大，<1 缩小。
        let scaled_size = ((line.size as f64) * config.font_size_scale).round() as i32;
        let font_size = scaled_size.max(1);
        let width = calc_text_width(&line.text, font_size);

        if line.mode == "R2L" {
            if let Some(pos) = normal.add(line.time, width as f64, font_size as f64, line.bottom) {
                let stime = pos.time;
                let dtime = config.scroll_time + stime;
                positioned.push(PositionedDanmaku {
                    text: line.text,
                    mode: "R2L".to_string(),
                    color: line.color,
                    stime,
                    dtime,
                    poss_x: PLAY_RES_X + width as f64 / 2.0,
                    poss_y: pos.top + font_size as f64,
                    posd_x: -width as f64 / 2.0,
                    posd_y: pos.top + font_size as f64,
                    font_size,
                });
            }
        } else if line.mode == "TOP" || line.mode == "BOTTOM" {
            let is_top = line.mode == "TOP";
            if let Some(pos) = side.add(line.time, font_size as f64, is_top, line.bottom) {
                let stime = pos.time;
                let dtime = config.fix_time + stime;
                let x = (PLAY_RES_X / 2.0).round();
                let y = pos.top + font_size as f64;
                positioned.push(PositionedDanmaku {
                    text: line.text,
                    mode: "Fix".to_string(),
                    color: line.color,
                    stime,
                    dtime,
                    poss_x: x,
                    poss_y: y,
                    posd_x: x,
                    posd_y: y,
                    font_size,
                });
            }
        }
    }

    positioned.sort_by(|a, b| {
        a.stime
            .partial_cmp(&b.stime)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    positioned
}

struct Pos {
    top: f64,
    time: f64,
}

#[derive(Clone)]
struct UsedItem {
    p: f64,
    m: f64,
    tf: f64,
    td: f64,
    b: bool,
}

struct Suggestion {
    p: f64,
    r: f64,
}

struct NormalDanmaku<'a> {
    used: Vec<UsedItem>,
    config: &'a BurnConfig,
}

impl<'a> NormalDanmaku<'a> {
    fn new(config: &'a BurnConfig) -> Self {
        Self {
            used: vec![
                UsedItem {
                    p: f64::NEG_INFINITY,
                    m: 0.0,
                    tf: f64::INFINITY,
                    td: f64::INFINITY,
                    b: false,
                },
                UsedItem {
                    p: PLAY_RES_Y,
                    m: f64::INFINITY,
                    tf: f64::INFINITY,
                    td: f64::INFINITY,
                    b: false,
                },
                UsedItem {
                    p: PLAY_RES_Y - config.bottom_reserve,
                    m: PLAY_RES_Y,
                    tf: f64::INFINITY,
                    td: f64::INFINITY,
                    b: true,
                },
            ],
            config,
        }
    }

    fn available(&self, hv: f64, t0s: f64, t0l: f64, b: bool) -> Vec<Suggestion> {
        let mut suggestion = Vec::new();
        for i in &self.used {
            if i.m > PLAY_RES_Y {
                continue;
            }
            let p = i.m;
            let m = p + hv;
            let mut tas = t0s;
            let mut tal = t0l;
            for j in &self.used {
                if j.p >= m {
                    continue;
                }
                if j.m <= p {
                    continue;
                }
                if j.b && b {
                    continue;
                }
                tas = tas.max(j.tf);
                tal = tal.max(j.td);
            }
            suggestion.push(Suggestion {
                p,
                r: (tas - t0s).max(tal - t0l),
            });
        }

        suggestion.sort_by(|a, b| a.p.partial_cmp(&b.p).unwrap_or(std::cmp::Ordering::Equal));
        let mut mr = MAX_DELAY;
        let mut result = Vec::new();
        for i in suggestion {
            if i.r >= mr {
                continue;
            }
            mr = i.r;
            result.push(i);
        }
        result
    }

    fn sync(&mut self, t0s: f64, t0l: f64) {
        self.used.retain(|i| i.tf > t0s || i.td > t0l);
    }

    fn score(&self, r: f64, p: f64) -> f64 {
        if r > MAX_DELAY {
            return f64::NEG_INFINITY;
        }
        1.0 - ((r / MAX_DELAY).powi(2) + (p / PLAY_RES_Y).powi(2)).sqrt() * 2.0f64.sqrt() / 2.0
    }

    fn add(&mut self, t0s: f64, wv: f64, hv: f64, b: bool) -> Option<Pos> {
        let t0l = PLAY_RES_X / (wv + PLAY_RES_X) * self.config.scroll_time + t0s;
        self.sync(t0s, t0l);
        let available = self.available(hv, t0s, t0l, b);
        if available.is_empty() {
            return None;
        }
        let best = available
            .into_iter()
            .map(|i| (self.score(i.r, i.p), i))
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))?
            .1;
        let ts = t0s + best.r;
        let tf = wv / (wv + PLAY_RES_X) * self.config.scroll_time + ts;
        let td = self.config.scroll_time + ts;
        self.used.push(UsedItem {
            p: best.p,
            m: best.p + hv,
            tf,
            td,
            b: false,
        });
        Some(Pos {
            top: best.p,
            time: ts,
        })
    }
}

struct SideDanmaku<'a> {
    used: Vec<UsedItem>,
    config: &'a BurnConfig,
}

impl<'a> SideDanmaku<'a> {
    fn new(config: &'a BurnConfig) -> Self {
        Self {
            used: vec![
                UsedItem {
                    p: f64::NEG_INFINITY,
                    m: 0.0,
                    tf: 0.0,
                    td: f64::INFINITY,
                    b: false,
                },
                UsedItem {
                    p: PLAY_RES_Y,
                    m: f64::INFINITY,
                    tf: 0.0,
                    td: f64::INFINITY,
                    b: false,
                },
                UsedItem {
                    p: PLAY_RES_Y - config.bottom_reserve,
                    m: PLAY_RES_Y,
                    tf: 0.0,
                    td: f64::INFINITY,
                    b: true,
                },
            ],
            config,
        }
    }

    fn fr(&self, p: f64, m: f64, t0s: f64, b: bool) -> Suggestion {
        let mut tas = t0s;
        for j in &self.used {
            if j.p >= m {
                continue;
            }
            if j.m <= p {
                continue;
            }
            if j.b && b {
                continue;
            }
            tas = tas.max(j.td);
        }
        Suggestion { p, r: tas - t0s }
    }

    fn top(&self, hv: f64, t0s: f64, b: bool) -> Vec<Suggestion> {
        let mut result = Vec::new();
        for i in &self.used {
            if i.m > PLAY_RES_Y {
                continue;
            }
            result.push(self.fr(i.m, i.m + hv, t0s, b));
        }
        result
    }

    fn bottom(&self, hv: f64, t0s: f64, b: bool) -> Vec<Suggestion> {
        let mut result = Vec::new();
        for i in &self.used {
            if i.p < 0.0 {
                continue;
            }
            result.push(self.fr(i.p - hv, i.p, t0s, b));
        }
        result
    }

    fn sync(&mut self, t0s: f64) {
        self.used.retain(|i| i.td > t0s);
    }

    fn score(&self, r: f64, p: f64, is_top: bool) -> f64 {
        if r > MAX_DELAY {
            return f64::NEG_INFINITY;
        }
        let f = if is_top { p } else { PLAY_RES_Y - p };
        1.0 - (r / MAX_DELAY * (31.0 / 32.0) + f / PLAY_RES_Y * (1.0 / 32.0))
    }

    fn add(&mut self, t0s: f64, hv: f64, is_top: bool, b: bool) -> Option<Pos> {
        self.sync(t0s);
        let available = if is_top {
            self.top(hv, t0s, b)
        } else {
            self.bottom(hv, t0s, b)
        };
        if available.is_empty() {
            return None;
        }
        let best = available
            .into_iter()
            .map(|i| (self.score(i.r, i.p, is_top), i))
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))?
            .1;
        self.used.push(UsedItem {
            p: best.p,
            m: best.p + hv,
            tf: 0.0,
            td: best.r + t0s + self.config.fix_time,
            b: false,
        });
        Some(Pos {
            top: best.p,
            time: best.r + t0s,
        })
    }
}

fn calc_text_width(text: &str, font_size: i32) -> i32 {
    let mut width = 0.0;
    for ch in text.chars() {
        if ch as u32 > 127 {
            width += font_size as f64;
        } else {
            width += font_size as f64 * 0.5;
        }
    }
    (width + SPACE) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calc_text_width() {
        let config = BurnConfig::default();
        assert_eq!(calc_text_width("abc", 25), 37); // 3 * 12.5 + 0 = 37.5 -> 37 (truncation)
        assert_eq!(calc_text_width("中文", 25), 50); // 2 * 25 = 50
                                                     // 验证 set_position 在默认 config 下不 panic
        let items = vec![DanmakuItem {
            text: "测试弹幕".to_string(),
            time: 1.0,
            mode: "R2L".to_string(),
            size: 25,
            color: "FFFFFF".to_string(),
            bottom: false,
        }];
        let positioned = set_position(&items, &config);
        assert!(!positioned.is_empty());
    }

    #[test]
    fn test_font_size_scale_applied() {
        let config = BurnConfig {
            font_size_scale: 2.0,
            ..Default::default()
        };
        let items = vec![DanmakuItem {
            text: "测试弹幕".to_string(),
            time: 1.0,
            mode: "R2L".to_string(),
            size: 25,
            color: "FFFFFF".to_string(),
            bottom: false,
        }];
        let positioned = set_position(&items, &config);
        // 字号应被放大到 50
        assert_eq!(positioned[0].font_size, 50);
    }
}
