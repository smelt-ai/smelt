// 技能面板纯函数（零宿主 UI 依赖，可直接 bun test 单测）。

export const BROKEN_PREVIEW = 5;

export function prettyPath(home, path) {
  if (!path) return "";
  if (!home) return path;
  const normHome = home.replace(/[\\/]+$/, "");
  if (path === normHome) return "~";
  if (path.startsWith(`${normHome}/`) || path.startsWith(`${normHome}\\`)) {
    return `~/${path.slice(normHome.length + 1)}`;
  }
  return path;
}

export function formatModified(modifiedMs, t) {
  if (typeof modifiedMs !== "number" || !Number.isFinite(modifiedMs)) {
    return t("time.unknown");
  }
  const d = new Date(modifiedMs);
  if (Number.isNaN(d.getTime())) return t("time.unknown");
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function joinNames(names, t) {
  return (names ?? []).join(t("list.separator"));
}

export function needsRepair(skill) {
  return (skill?.missing_bridges?.length ?? 0) > 0 && (skill?.conflicts?.length ?? 0) === 0;
}

export function scopeRequestOf(skill) {
  if (skill?.placement?.kind === "agent" && skill.placement.label) {
    return {
      universal: false,
      labels: [skill.placement.label],
      project_scope: Boolean(skill.project_scope),
    };
  }
  return {
    universal: true,
    labels: [],
    project_scope: Boolean(skill?.project_scope),
  };
}

/** 当前层级允许单独放置技能的 agent，在用的排前面。 */
export function scopeChoices(agents, installed, projectScope) {
  const installedSet = new Set(installed ?? []);
  const choices = [];
  for (const agent of agents ?? []) {
    const hasDir = projectScope ? agent.has_project_dir : agent.has_user_dir;
    if (!hasDir) continue;
    choices.push({ label: agent.label, installed: installedSet.has(agent.label) });
  }
  choices.sort((a, b) => Number(b.installed) - Number(a.installed));
  return choices;
}

export function filterSkills(skills, rawQuery) {
  const query = String(rawQuery ?? "").trim().toLowerCase();
  if (!query) return skills ?? [];
  return (skills ?? []).filter(
    (s) =>
      String(s.name ?? "").toLowerCase().includes(query) ||
      String(s.description ?? "").toLowerCase().includes(query),
  );
}

export function placeLabel(placement, t) {
  if (!placement || placement.kind === "universal") return t("place.universal");
  if (placement.kind === "legacy") return t("place.legacy");
  return placement.label ?? "";
}

export function skillChip(skill, t) {
  const conflicts = skill?.conflicts?.length ?? 0;
  if (conflicts > 0) {
    return { label: t("place.conflict", { count: conflicts + 1 }), tone: "bad" };
  }
  if (skill?.placement?.kind === "legacy") {
    return { label: t("place.legacy"), tone: "need" };
  }
  if (skill?.placement?.kind === "agent") {
    return { label: skill.placement.label ?? "", tone: "neutral" };
  }
  return { label: t("place.universal"), tone: "neutral" };
}

export function skillSummary(skill, t) {
  if ((skill?.conflicts?.length ?? 0) > 0) {
    return { label: t("status.choose"), tone: "bad", badge: false };
  }
  if (skill?.placement?.kind === "legacy") {
    return { label: t("status.unread"), tone: "need", badge: true };
  }
  const missing = (skill?.coverage ?? [])
    .filter((row) => row.access === "missing")
    .map((row) => row.agent);
  if (missing.length === 0) {
    const count = skill?.coverage?.length ?? 0;
    return {
      label: count === 1 ? t("status.agents.one") : t("status.agents.other", { count }),
      tone: "muted",
      badge: false,
    };
  }
  if (missing.length === 1) {
    return { label: t("status.missing_one", { agent: missing[0] }), tone: "need", badge: true };
  }
  return { label: t("status.missing_many", { count: missing.length }), tone: "need", badge: true };
}

export function describeAction(home, action, t) {
  switch (action?.kind) {
    case "install":
      return t("step.copy", { path: prettyPath(home, action.to) });
    case "move":
      return t("step.move", {
        from: prettyPath(home, action.from),
        to: prettyPath(home, action.to),
      });
    case "link":
      return action.agent
        ? t("step.link_for", { agent: action.agent, path: prettyPath(home, action.at) })
        : t("step.link", { path: prettyPath(home, action.at) });
    case "unlink":
      return action.agent
        ? t("step.unlink_for", { agent: action.agent, path: prettyPath(home, action.at) })
        : t("step.unlink", { path: prettyPath(home, action.at) });
    case "trash":
      return t("step.trash", { path: prettyPath(home, action.from) });
    default:
      return "";
  }
}

export function migrationNotice(outcome, t) {
  const migrated = outcome?.migrated ?? [];
  const skipped = outcome?.skipped ?? [];
  const lines = [];
  if (migrated.length > 0) {
    lines.push(
      migrated.length === 1
        ? t("migrate.moved.one", { names: joinNames(migrated, t) })
        : t("migrate.moved.other", { count: migrated.length, names: joinNames(migrated, t) }),
    );
  }
  if (skipped.length > 0) {
    lines.push(
      skipped.length === 1
        ? t("migrate.skipped.one", { names: joinNames(skipped, t) })
        : t("migrate.skipped.other", { count: skipped.length, names: joinNames(skipped, t) }),
    );
  }
  if (lines.length === 0) {
    lines.push(t("migrate.none"));
  }
  return {
    error: skipped.length > 0,
    text: lines.join("\n"),
  };
}

export function brokenConfirmBody(home, broken, t) {
  const list = broken ?? [];
  const preview = list.slice(0, BROKEN_PREVIEW).map((link) => prettyPath(home, link.path));
  if (list.length > preview.length) {
    preview.push(t("broken.more", { count: list.length }));
  }
  return t("broken.confirm_body", { paths: preview.join("\n") });
}
