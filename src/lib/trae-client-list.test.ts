import { describe, expect, it } from "vitest";

import {
  firstSelectableKey,
  isSelectableClient,
  resolveSelectableKey,
  selectableClients,
  type SelectableClientLike,
} from "./trae-client-list";

/**
 * 真机形态（2026-10 官方把 TRAE SOLO 并进 Trae 之后）：
 * SOLO **数据目录还在**（`installed = true`）但**没有自己的程序**（`runnable = false`），
 * Trae 一切正常。用这组当基准，别用臆造的形态。
 *
 * ⚠️ 合并是**两条线同时**发生的：国内版 `solo-cn → trae-cn`，国际版 `solo-intl → trae-intl`。
 *    只摘国内版会出现「同一件事只做了一半」—— 用户看到的就是国际版 SOLO 还挂在标签条上。
 */
const soloMerged = {
  key: "solo-cn",
  installed: true,
  runnable: false,
  merged_into: "trae-cn",
} satisfies SelectableClientLike;

const hostCn = {
  key: "trae-cn",
  installed: true,
  runnable: true,
  merged_into: null,
} satisfies SelectableClientLike;

/** 国际版 SOLO 并入国际版 Trae；本机没有它的数据目录（也没程序）。 */
const soloIntlMerged = {
  key: "solo-intl",
  installed: false,
  runnable: false,
  merged_into: "trae-intl",
} satisfies SelectableClientLike;

/** 国际版 Trae：没装（置灰）但**没被并入**，不属于该摘的对象。 */
const hostIntl = {
  key: "trae-intl",
  installed: false,
  runnable: false,
  merged_into: null,
} satisfies SelectableClientLike;

/** 老机器：SOLO 还独立装着。 */
const soloStandalone = {
  key: "solo-cn",
  installed: true,
  runnable: true,
  merged_into: "trae-cn",
} satisfies SelectableClientLike;

describe("isSelectableClient", () => {
  it("已并入且本机跑不起来 ⇒ 不进选择器（合并后的 SOLO）", () => {
    expect(isSelectableClient(soloMerged)).toBe(false);
  });

  it("国际版 SOLO 走同一条判据（合并是两条线，别只摘国内版）", () => {
    expect(isSelectableClient(soloIntlMerged)).toBe(false);
    // 数据目录还在、只是没程序的那种同样摘掉。
    expect(isSelectableClient({ ...soloIntlMerged, installed: true })).toBe(false);
  });

  it("国际版 Trae 只是「没装」，不是「已并入」⇒ 不连坐", () => {
    expect(isSelectableClient(hostIntl)).toBe(true);
  });

  it("已并入但本机还装着它自己的程序 ⇒ 仍然能选（老机器，别砍功能）", () => {
    expect(isSelectableClient(soloStandalone)).toBe(true);
  });

  it("老后端没下发 runnable ⇒ 按「能选」处理，不误伤", () => {
    expect(isSelectableClient({ key: "solo-cn", installed: true, merged_into: "trae-cn" })).toBe(
      true,
    );
  });

  it("未安装且未并入 ⇒ 照旧出现（置灰是既有的约定）", () => {
    expect(isSelectableClient({ key: "trae-intl", installed: false })).toBe(true);
  });

  it("总览结构的 camelCase 字段同样生效", () => {
    expect(
      isSelectableClient({ key: "solo-cn", installed: true, runnable: false, mergedInto: "trae-cn" }),
    ).toBe(false);
    expect(
      isSelectableClient({ key: "solo-cn", installed: true, runnable: true, mergedInto: "trae-cn" }),
    ).toBe(true);
  });

  it("空串 / undefined 都算「没并入」，不会被误摘", () => {
    expect(isSelectableClient({ key: "trae-cn", installed: true, runnable: true, merged_into: "" })).toBe(
      true,
    );
    expect(
      isSelectableClient({ key: "trae-cn", installed: true, runnable: true, merged_into: undefined }),
    ).toBe(true);
  });

  it("已并入 + 明确 false 才算摘；runnable 真值一律保留", () => {
    // runnable 只有**严格等于 false** 才判「跑不起来」，别把 undefined 当 false。
    expect(isSelectableClient({ ...soloMerged, runnable: undefined })).toBe(true);
  });
});

describe("selectableClients", () => {
  it("只摘掉该项，顺序原样保留（顺序只归后端）", () => {
    const list = selectableClients([soloMerged, hostCn]);
    expect(list.map((c) => c.key)).toEqual(["trae-cn"]);
  });

  it("没有可摘项时原样返回全部", () => {
    const list = selectableClients([soloStandalone, hostCn]);
    expect(list.map((c) => c.key)).toEqual(["solo-cn", "trae-cn"]);
  });

  it("真机四客户端形态（后端排序后）⇒ 只剩两个 IDE，两条 SOLO 都不在", () => {
    // 顺序复刻后端 `sort_with_order`：可运行的在前，其余按使用记忆/内置偏好。
    const list = selectableClients([hostCn, soloMerged, soloIntlMerged, hostIntl]);
    expect(list.map((c) => c.key)).toEqual(["trae-cn", "trae-intl"]);
  });
});

describe("firstSelectableKey", () => {
  it("跳过被摘掉的项（后端仍把它排第一，取值时得能跨过去）", () => {
    expect(firstSelectableKey([soloMerged, hostCn])).toBe("trae-cn");
  });

  it("一个可见的都没有 ⇒ null（不是抛错、也不是返回死 key）", () => {
    expect(firstSelectableKey([soloMerged])).toBeNull();
  });
});

describe("resolveSelectableKey", () => {
  it("记忆里存的是已被摘掉的客户端 ⇒ 回落到可见首项（选中态不悬空）", () => {
    expect(resolveSelectableKey("solo-cn", [soloMerged, hostCn])).toBe("trae-cn");
  });

  it("记忆里的客户端仍可见 ⇒ 保留（不无谓地跳走）", () => {
    expect(resolveSelectableKey("trae-cn", [soloMerged, hostCn])).toBe("trae-cn");
  });

  it("没有记忆 ⇒ 取可见首项", () => {
    expect(resolveSelectableKey(null, [soloMerged, hostCn])).toBe("trae-cn");
  });

  it("全部都不可见 ⇒ null，别再把记忆里的死 key 传下去", () => {
    expect(resolveSelectableKey("solo-cn", [soloMerged])).toBeNull();
  });

  it("空列表 ⇒ null", () => {
    expect(resolveSelectableKey("trae-cn", [])).toBeNull();
  });
});
