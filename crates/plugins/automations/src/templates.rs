//! 自动化模板目录。
//!
//! 目录页只消费 [`AutomationTemplate`]，不关心来源。内置模板是产品预填，
//! 方便从空白编辑器起步；后续若插件要注册模板，往同一份结构靠即可。

use api::model::automation::{
    AutomationSchedule, SCHEDULE_DAY_FRI, SCHEDULE_DAY_MON, SCHEDULE_DAY_WED,
};
use gpui::SharedString;
use rust_i18n::t;
use smelt_plugin_api as api;
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationTemplateCategory {
    Work,
    Engineering,
    Research,
}

impl AutomationTemplateCategory {
    pub(crate) const ALL: [Self; 3] = [Self::Work, Self::Engineering, Self::Research];

    pub(crate) fn label(self) -> SharedString {
        match self {
            Self::Work => t!("template.work"),
            Self::Engineering => t!("template.engineering"),
            Self::Research => t!("template.research"),
        }
        .into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationTemplateGlyph {
    Sun,
    Calendar,
    Inbox,
    File,
    Cpu,
    Globe,
    BookOpen,
    LayoutDashboard,
    TriangleAlert,
}

/// 文案按当前语言现取。
type TemplateText = fn() -> Cow<'static, str>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AutomationTemplate {
    pub id: &'static str,
    pub name: TemplateText,
    pub description: TemplateText,
    /// 指令里用方括号（中文是「填写…」）留出要用户补的路径或对象。
    pub prompt: TemplateText,
    pub category: AutomationTemplateCategory,
    pub glyph: AutomationTemplateGlyph,
    pub schedule: AutomationSchedule,
}

pub(crate) const BUILTIN: &[AutomationTemplate] = &[
    AutomationTemplate {
        id: "morning-brief",
        name: || t!("template.morning_brief.name"),
        description: || t!("template.morning_brief.description"),
        prompt: || t!("template.morning_brief.prompt"),
        category: AutomationTemplateCategory::Work,
        glyph: AutomationTemplateGlyph::Sun,
        schedule: AutomationSchedule::Daily {
            hour: 7,
            minute: 30,
        },
    },
    AutomationTemplate {
        id: "weekly-review",
        name: || t!("template.weekly_review.name"),
        description: || t!("template.weekly_review.description"),
        prompt: || t!("template.weekly_review.prompt"),
        category: AutomationTemplateCategory::Work,
        glyph: AutomationTemplateGlyph::Calendar,
        schedule: AutomationSchedule::Weekly {
            days: SCHEDULE_DAY_FRI,
            hour: 16,
            minute: 0,
        },
    },
    AutomationTemplate {
        id: "daily-planner",
        name: || t!("template.daily_planner.name"),
        description: || t!("template.daily_planner.description"),
        prompt: || t!("template.daily_planner.prompt"),
        category: AutomationTemplateCategory::Work,
        glyph: AutomationTemplateGlyph::LayoutDashboard,
        schedule: AutomationSchedule::Daily {
            hour: 8,
            minute: 30,
        },
    },
    AutomationTemplate {
        id: "task-extractor",
        name: || t!("template.task_extractor.name"),
        description: || t!("template.task_extractor.description"),
        prompt: || t!("template.task_extractor.prompt"),
        category: AutomationTemplateCategory::Work,
        glyph: AutomationTemplateGlyph::Inbox,
        schedule: AutomationSchedule::Daily {
            hour: 17,
            minute: 0,
        },
    },
    AutomationTemplate {
        id: "pending-changes",
        name: || t!("template.pending_changes.name"),
        description: || t!("template.pending_changes.description"),
        prompt: || t!("template.pending_changes.prompt"),
        category: AutomationTemplateCategory::Engineering,
        glyph: AutomationTemplateGlyph::File,
        schedule: AutomationSchedule::Daily { hour: 9, minute: 0 },
    },
    AutomationTemplate {
        id: "dependency-check",
        name: || t!("template.dependency_check.name"),
        description: || t!("template.dependency_check.description"),
        prompt: || t!("template.dependency_check.prompt"),
        category: AutomationTemplateCategory::Engineering,
        glyph: AutomationTemplateGlyph::TriangleAlert,
        schedule: AutomationSchedule::Weekly {
            days: SCHEDULE_DAY_MON,
            hour: 9,
            minute: 0,
        },
    },
    AutomationTemplate {
        id: "competitor-watch",
        name: || t!("template.competitor_watch.name"),
        description: || t!("template.competitor_watch.description"),
        prompt: || t!("template.competitor_watch.prompt"),
        category: AutomationTemplateCategory::Research,
        glyph: AutomationTemplateGlyph::Globe,
        schedule: AutomationSchedule::Weekly {
            days: SCHEDULE_DAY_MON,
            hour: 9,
            minute: 0,
        },
    },
    AutomationTemplate {
        id: "tech-digest",
        name: || t!("template.tech_digest.name"),
        description: || t!("template.tech_digest.description"),
        prompt: || t!("template.tech_digest.prompt"),
        category: AutomationTemplateCategory::Research,
        glyph: AutomationTemplateGlyph::Cpu,
        schedule: AutomationSchedule::Daily { hour: 8, minute: 0 },
    },
    AutomationTemplate {
        id: "paper-watch",
        name: || t!("template.paper_watch.name"),
        description: || t!("template.paper_watch.description"),
        prompt: || t!("template.paper_watch.prompt"),
        category: AutomationTemplateCategory::Research,
        glyph: AutomationTemplateGlyph::BookOpen,
        schedule: AutomationSchedule::Weekly {
            days: SCHEDULE_DAY_WED,
            hour: 9,
            minute: 0,
        },
    },
];

pub(crate) fn builtin_automation_templates() -> &'static [AutomationTemplate] {
    BUILTIN
}

pub(crate) fn automation_template(id: &str) -> Option<&'static AutomationTemplate> {
    BUILTIN.iter().find(|template| template.id == id)
}

pub(crate) fn filtered_templates(
    filter: Option<AutomationTemplateCategory>,
) -> impl Iterator<Item = &'static AutomationTemplate> {
    builtin_automation_templates()
        .iter()
        .filter(move |template| filter.is_none_or(|category| template.category == category))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn builtin_templates_have_stable_unique_ids_and_copy() {
        let mut ids = HashSet::new();
        for template in BUILTIN {
            assert!(!template.id.is_empty());
            assert!(!(template.name)().is_empty(), "{}", template.id);
            assert!(!(template.description)().is_empty(), "{}", template.id);
            let prompt = (template.prompt)();
            assert!(
                prompt.contains('['),
                "{}：运行在空目录里，指令必须留出要读的路径或对象",
                template.id
            );
            assert!(
                ids.insert(template.id),
                "duplicate template id {}",
                template.id
            );
        }
        assert_eq!(BUILTIN.len(), 9);
        assert!(automation_template("morning-brief").is_some());
        assert!(automation_template("missing").is_none());
    }

    #[test]
    fn every_category_has_templates_and_all_filter_is_complete() {
        for category in AutomationTemplateCategory::ALL {
            let count = filtered_templates(Some(category)).count();
            assert!(count > 0, "{}", category.label());
            assert_eq!(
                count,
                BUILTIN
                    .iter()
                    .filter(|template| template.category == category)
                    .count()
            );
        }
        assert_eq!(filtered_templates(None).count(), BUILTIN.len());
    }

    #[test]
    fn template_schedules_use_product_summaries() {
        use crate::format::schedule_summary;
        let morning = automation_template("morning-brief").unwrap();
        assert_eq!(schedule_summary(&morning.schedule), "Daily 07:30");
        let weekly = automation_template("weekly-review").unwrap();
        assert_eq!(schedule_summary(&weekly.schedule), "Every Fri · 16:00");
        let monday = automation_template("dependency-check").unwrap();
        assert_eq!(schedule_summary(&monday.schedule), "Every Mon · 09:00");
    }
}
