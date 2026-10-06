import { WorkbuddyImportCard } from "@/components/workbuddy-import-card";

/**
 * WorkBuddy 会话导入（独立页面）。
 *
 * 此前这块能力被塞在「Trae 会话记录」页底部，与页面的客户端 / 解密 / 会话列表主线混在一起。
 * 它本质是「把外部工具的明文会话迁移进 Trae」的独立工作流，因此拆成单独路由，
 * 侧栏单独占一个区域。这里只负责页面骨架，业务逻辑仍在 {@link WorkbuddyImportCard}。
 */
export default function WorkbuddyImportPage() {
  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">WorkBuddy → Trae</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          读取本机 WorkBuddy 的明文会话（JSONL），转换成 Trae 的关系库结构后加密写入指定账号的本地库——
          包含提问、最终回答，以及思考与工具调用过程。
        </p>
      </div>

      <WorkbuddyImportCard defaultClientKey={null} />
    </div>
  );
}
