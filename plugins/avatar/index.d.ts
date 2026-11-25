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
}

/** 演一遍就停的那几出演多久（毫秒）：过了这么久头像自己站好。 */
export declare const ONE_SHOT_MS: Readonly<Record<"cheer" | "greet", number>>;

export declare function Avatar(props: AvatarProps): JSX.Element;
