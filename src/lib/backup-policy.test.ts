import { describe, expect, it } from "vitest";

import {
  KEEP_COUNT_CAP,
  KEEP_DAYS_CAP,
  MAX_TOTAL_MB_CAP,
  clampInt,
  gbInputToMb,
  humanMb,
  mbToGbInput,
} from "./backup-policy";

describe("clampInt", () => {
  it("把输入夹进区间（与后端 sanitized 的上限一致）", () => {
    expect(clampInt("30", 0, KEEP_DAYS_CAP)).toBe(30);
    expect(clampInt("0", 0, KEEP_DAYS_CAP)).toBe(0);
    expect(clampInt("999999999", 0, KEEP_DAYS_CAP)).toBe(KEEP_DAYS_CAP);
    expect(clampInt("-5", 0, KEEP_DAYS_CAP)).toBe(0);
    expect(clampInt("3", 0, KEEP_COUNT_CAP)).toBe(3);
    expect(clampInt("99999", 0, KEEP_COUNT_CAP)).toBe(KEEP_COUNT_CAP);
  });

  it("空串 / 非数字按 min 算，不抛异常也不给 NaN", () => {
    // ⚠️ 用户把输入框清空的瞬间会拿到 ""；返回 NaN 会让受控 input 变成
    //    「value=NaN」的 React 警告 + 输入框卡死。
    expect(clampInt("", 0, KEEP_DAYS_CAP)).toBe(0);
    expect(clampInt("   ", 0, KEEP_DAYS_CAP)).toBe(0);
    expect(clampInt("abc", 0, KEEP_DAYS_CAP)).toBe(0);
    expect(Number.isNaN(clampInt("abc", 0, 99))).toBe(false);
  });

  it("小数按整数部分取（输入框 step=1）", () => {
    expect(clampInt("7.9", 0, KEEP_DAYS_CAP)).toBe(7);
  });
});

describe("GB ↔ MB 换算", () => {
  it("默认值 4096 MB 显示成 4 GB，并能原样换回去", () => {
    // ★ 这是最要命的一对：换算错了会把 4 GB 静默存成 4 MB，
    //   策略随即把几乎所有备份都判成「超容量」。
    expect(mbToGbInput(4096)).toBe(4);
    expect(gbInputToMb("4")).toBe(4096);
    expect(gbInputToMb(String(mbToGbInput(4096)))).toBe(4096);
  });

  it("半 GB 也自洽", () => {
    expect(mbToGbInput(512)).toBe(0.5);
    expect(gbInputToMb("0.5")).toBe(512);
  });

  it("空 / 0 / 负数一律 0（= 不按容量判，而不是「全清掉」）", () => {
    // ⚠️ 清空输入框的本意是「不限制」。若当成 0 MB 上限，下一秒就会把备份全删光。
    expect(gbInputToMb("")).toBe(0);
    expect(gbInputToMb("   ")).toBe(0);
    expect(gbInputToMb("0")).toBe(0);
    expect(gbInputToMb("-3")).toBe(0);
    expect(gbInputToMb("abc")).toBe(0);
  });

  it("上界与后端一致（1 TB）", () => {
    expect(gbInputToMb("999999")).toBe(MAX_TOTAL_MB_CAP);
    expect(MAX_TOTAL_MB_CAP).toBe(1_048_576);
  });

  it("输入 0 MB 时显示 0 而不是空", () => {
    expect(mbToGbInput(0)).toBe(0);
    expect(String(mbToGbInput(0))).toBe("0");
  });
});

describe("humanMb", () => {
  it("1024 MB 以上换 GB", () => {
    expect(humanMb(4096)).toBe("4 GB");
    expect(humanMb(512)).toBe("512 MB");
    expect(humanMb(1536)).toBe("1.5 GB");
  });
});
