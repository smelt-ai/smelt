import type { JSX } from "../nsh/types";

/** 演哪一出：站着、干活、喊你、欢呼一下、打个招呼。 */
export type AvatarAct = "rest" | "work" | "call" | "cheer" | "greet";

export interface AvatarProps {
  /** 按它算外形、颜色和性格，同一个 seed 永远同一个头像。 */
  seed: string;
  act: AvatarAct | (string & {});
  /** 这一出从什么时候起（Unix 毫秒）；演一遍的那几出过了时长就站好不演。 */
  since: number;
  size: number;
  opacity?: number;
  /** 眼睛用的图标路径（比如智能体图标）：两只眼都画成它，和圆点眼同色；不给就是圆点眼。 */
  eye?: string;
  /** 是否可见（折叠分组、隐藏侧栏或非活动页时传 false，暂停动画）。 */
  active?: boolean;
}

/** 演一遍就停的那几出演多久（毫秒）：过了这么久头像自己站好。 */
export declare const ONE_SHOT_MS: Readonly<Record<"cheer" | "greet", number>>;

export declare function Avatar(props: AvatarProps): JSX.Element;
