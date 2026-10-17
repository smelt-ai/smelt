//! 自动化模板目录。
//!
//! 目录页只消费 [`AutomationTemplate`]，不关心来源。内置模板是产品预填，
//! 方便从空白编辑器起步；后续若插件要注册模板，往同一份结构靠即可。

use smelt_model::automation::{
    AutomationSchedule, SCHEDULE_DAY_FRI, SCHEDULE_DAY_MON, SCHEDULE_DAY_WED,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationTemplateCategory {
    Work,
    Engineering,
    Research,
}

impl AutomationTemplateCategory {
    pub(super) const ALL: [Self; 3] = [Self::Work, Self::Engineering, Self::Research];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Work => "工作",
            Self::Engineering => "工程",
            Self::Research => "研究",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AutomationTemplateGlyph {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AutomationTemplate {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub prompt: &'static str,
    pub category: AutomationTemplateCategory,
    pub glyph: AutomationTemplateGlyph,
    pub schedule: AutomationSchedule,
}

const BUILTIN: &[AutomationTemplate] = &[
    AutomationTemplate {
        id: "morning-brief",
        name: "每日晨报",
        description: "用几条要点打开今天：仓库动态，以及最该先做的三件事。",
        prompt: "查看仓库「填写仓库路径」过去一天的提交、分支和未提交变更，用简短条目列出今天最该先处理的三件事。不要写成散文。",
        category: AutomationTemplateCategory::Work,
        glyph: AutomationTemplateGlyph::Sun,
        schedule: AutomationSchedule::Daily {
            hour: 7,
            minute: 30,
        },
    },
    AutomationTemplate {
        id: "weekly-review",
        name: "每周回顾",
        description: "按本周的提交收束完成项、卡住的问题，和下周只保留的三条优先级。",
        prompt: "回顾仓库「填写仓库路径」本周的提交和合并：做成了什么、卡在哪里、下周只保留三条优先级。用条目，不要套话。",
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
        name: "每日计划",
        description: "按你的待办文件排出今天可执行的时间块和快赢项。",
        prompt: "读取待办文件「填写文件路径」，排出今天可执行的时间块。每块写目标和完成标准，先快赢再深水区。",
        category: AutomationTemplateCategory::Work,
        glyph: AutomationTemplateGlyph::LayoutDashboard,
        schedule: AutomationSchedule::Daily {
            hour: 8,
            minute: 30,
        },
    },
    AutomationTemplate {
        id: "task-extractor",
        name: "待办提取",
        description: "从笔记目录里最近改过的文件抽出可执行下一步，去重后分组。",
        prompt: "读取笔记目录「填写目录路径」里最近一天改过的文件，提取可执行待办，去重、合并相似项，按今天/本周/以后分组。",
        category: AutomationTemplateCategory::Work,
        glyph: AutomationTemplateGlyph::Inbox,
        schedule: AutomationSchedule::Daily {
            hour: 17,
            minute: 0,
        },
    },
    AutomationTemplate {
        id: "pending-changes",
        name: "待审变更",
        description: "检查仓库里待审的合并请求和未提交变更，标出该先看的风险。",
        prompt: "检查仓库「填写仓库路径」里待审的合并请求和未提交变更。标出风险文件、测试缺口，以及建议先看的顺序。",
        category: AutomationTemplateCategory::Engineering,
        glyph: AutomationTemplateGlyph::File,
        schedule: AutomationSchedule::Daily { hour: 9, minute: 0 },
    },
    AutomationTemplate {
        id: "dependency-check",
        name: "依赖体检",
        description: "只报告过期或高风险依赖，并给出最小修复建议。",
        prompt: "检查仓库「填写仓库路径」的依赖是否过期或有已知高风险问题。只报告需要处理的项，并给出最小修复建议。",
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
        name: "竞品观察",
        description: "汇总关注对象本周的产品、定价和公开动态。",
        prompt: "汇总「填写关注的产品或公司」本周的产品、定价和公开动态。每条写清来源、变化、对我们的含义。",
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
        name: "技术动态",
        description: "挑你关注方向的模型、框架、工具发布，说明要不要跟。",
        prompt: "汇总过去一天里「填写关注的技术方向」相关的模型、框架、工具发布。每条说明影响和是否值得跟进。",
        category: AutomationTemplateCategory::Research,
        glyph: AutomationTemplateGlyph::Cpu,
        schedule: AutomationSchedule::Daily { hour: 8, minute: 0 },
    },
    AutomationTemplate {
        id: "paper-watch",
        name: "论文速读",
        description: "用白话讲清本周最值得看的一篇研究，以及能不能用上。",
        prompt: "挑选本周「填写研究方向」最值得看的一篇研究或技术文章，用白话讲方法和结论，以及能不能用在我们的工作里。",
        category: AutomationTemplateCategory::Research,
        glyph: AutomationTemplateGlyph::BookOpen,
        schedule: AutomationSchedule::Weekly {
            days: SCHEDULE_DAY_WED,
            hour: 9,
            minute: 0,
        },
    },
];

pub(super) fn builtin_automation_templates() -> &'static [AutomationTemplate] {
    BUILTIN
}

pub(super) fn automation_template(id: &str) -> Option<&'static AutomationTemplate> {
    BUILTIN.iter().find(|template| template.id == id)
}

pub(super) fn filtered_templates(
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
            assert!(!template.id.is_empty(), "{}", template.name);
            assert!(!template.name.is_empty(), "{}", template.id);
            assert!(!template.description.is_empty(), "{}", template.id);
            assert!(!template.prompt.is_empty(), "{}", template.id);
            assert!(
                template.prompt.contains("「填写"),
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
        let morning = automation_template("morning-brief").unwrap();
        assert_eq!(morning.schedule.summary(), "每天 07:30");
        let weekly = automation_template("weekly-review").unwrap();
        assert_eq!(weekly.schedule.summary(), "每周五 · 16:00");
        let monday = automation_template("dependency-check").unwrap();
        assert_eq!(monday.schedule.summary(), "每周一 · 09:00");
    }
}
