import { describe, expect, it } from "vitest";

import {
  firstSelectableKey,
  isSelectableClient,
  resolveSelectableKey,
  selectableClients,
  type SelectableClientLike,
} from "./trae-client-list";

/**
 * 真机形态（2026-10 官方把 TRAE SOLO 并进 Trae CN 之后）：
 * SOLO **数据目录还在**（`installed = true`）但**没有自己的程序**（`runnable = false`），
 * Trae CN 一切正常。用这组当基准，别用臆造的形态。
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
