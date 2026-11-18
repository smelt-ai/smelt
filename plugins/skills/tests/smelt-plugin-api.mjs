// 测试用的 `smelt-plugin-api`：没有界面，选路径一律当成取消，界面语言是英文。

export function locale() {
  return "en";
}

export function run(command, _args) {
  if (command === "smelt.app/pick-paths") return Promise.resolve(null);
  return Promise.reject(new Error(`测试里没有命令 ${command}`));
}
