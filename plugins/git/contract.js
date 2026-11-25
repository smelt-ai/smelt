// 变更（来源 `smelt.git`）对外的约定：别的插件能调的命令。
//
// 这里是权威定义。插件之间不互相依赖：调用方各自照抄用到的 id 和字段，注明出处。
// 改这里时同一次改动里把照抄的各处一起改掉（`grep -rn 'smelt.git/' crates plugins`）。

/**
 * 在浏览器里打开某个仓库当前分支的新建 PR 页。
 * 参数：`{ machine: string | null, root: string }`
 * 开不了时（还没推送、就在默认分支、认不出远端）命令失败，原因给人看。
 */
export const OPEN_PULL_REQUEST = "smelt.git/open-pull-request";
