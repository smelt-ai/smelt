/// <reference types="bun" />
// 包发布的是 TS 源码，消费者的 tsc 会直接编译 src/。源码用到 node:fs / node:net /
// Buffer / process 这些 ambient 声明，靠消费者自己在 tsconfig 里写 types: ["bun"]
// 才能过——那等于把我们的实现细节变成对方的配置负担，README 抄下来就报错。
// 这条三斜线引用把这个需求带进包内，消费者零配置即可。

export * from "./types.js";
