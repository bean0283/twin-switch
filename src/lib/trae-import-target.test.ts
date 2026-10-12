import { describe, expect, test } from "vitest";

import { defaultTargetClient, targetClientOptions } from "@/lib/trae-import-target";
import type { TraeInstalledClient } from "@/lib/trae-types";

/** 造一个客户端；只写关心的字段，其余按「能跑」的常态补齐。 */
function client(
  key: string,
  extra: Partial<TraeInstalledClient> = {},
): TraeInstalledClient {
  return {
    key,
    label: key,
    user_data_dir: `C:/x/${key}`,
    installed: true,
    exe: `C:/x/${key}/${key}.exe`,
    has_login: true,
    merged_into: null,
    runnable: true,
    ...extra,
  };
}

/** 真机形态：SOLO 的数据目录还在（`installed`），但已经没有自己的程序了。 */
const mergedSolo = client("solo-cn", {
  exe: null,
  merged_into: "trae-cn",
  runnable: false,
});
const hostCn = client("trae-cn");

describe("targetClientOptions", () => {
  test("过滤掉「已并入、没有自己程序」的客户端", () => {
    const list = targetClientOptions([mergedSolo, hostCn]);
    expect(list.map((c) => c.key)).toEqual(["trae-cn"]);
  });

  test("未安装的也不列", () => {
    const notInstalled = client("solo-intl", { installed: false, runnable: false });
    expect(targetClientOptions([notInstalled, hostCn]).map((c) => c.key)).toEqual(["trae-cn"]);
  });

  test("老后端没有 runnable 字段时按「能用」处理，不能整套筛空", () => {
    const legacy = client("trae-cn");
    delete (legacy as { runnable?: boolean }).runnable;
    expect(targetClientOptions([legacy]).map((c) => c.key)).toEqual(["trae-cn"]);
  });
});

describe("defaultTargetClient", () => {
  test("源客户端自己还能用 ⇒ 仍选它（同客户端跨账号复制是最常用用法）", () => {
    expect(defaultTargetClient("trae-cn", [mergedSolo, hostCn])).toBe("trae-cn");
  });

  test("源已并入别人 ⇒ 落到承载方，不能落回源自己", () => {
    // 这条就是本次修复的核心：SOLO 的使用分最高、旧逻辑必然选它，
    // 于是「导入」会把会话写进一个客户端永远不读的库。
    expect(defaultTargetClient("solo-cn", [mergedSolo, hostCn])).toBe("trae-cn");
  });

  test("源不可用且没有承载方 ⇒ 退到第一个能用的", () => {
    const orphan = client("solo-intl", { runnable: false, merged_into: null });
    expect(defaultTargetClient("solo-intl", [orphan, hostCn])).toBe("trae-cn");
  });

  test("一个能用的都没有 ⇒ null（界面按「没有目标」处理）", () => {
    expect(defaultTargetClient("solo-cn", [mergedSolo])).toBeNull();
  });

  test("还没选源（clientKey 为 null）⇒ 第一个能用的", () => {
    expect(defaultTargetClient(null, [mergedSolo, hostCn])).toBe("trae-cn");
  });

  test("承载方自己也不可用 ⇒ 不硬选它，退到下一个能用的", () => {
    const deadHost = client("trae-cn", { runnable: false });
    const other = client("trae-intl");
    expect(defaultTargetClient("solo-cn", [mergedSolo, deadHost, other])).toBe("trae-intl");
  });

  test("返回值一定在可选列表里 —— 两处判据同源", () => {
    // 防的是「下拉里没有默认选中的那一项」这种自相矛盾的界面：
    // 只要两个函数各写一套判据，早晚会分叉。
    const lists: TraeInstalledClient[][] = [
      [mergedSolo, hostCn],
      [mergedSolo],
      [mergedSolo, client("trae-cn", { runnable: false }), client("trae-intl")],
      [client("trae-intl", { installed: false }), hostCn],
    ];
    for (const list of lists) {
      for (const src of [null, "solo-cn", "trae-cn", "trae-intl", "不存在"]) {
        const picked = defaultTargetClient(src, list);
        if (picked === null) continue;
        expect(targetClientOptions(list).map((c) => c.key)).toContain(picked);
      }
    }
  });
});
