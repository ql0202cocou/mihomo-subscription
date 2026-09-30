import { useCallback } from "react";
import { App as AntdApp } from "antd";
import { useTranslation } from "react-i18next";
import type { Regenerate } from "../types";

/** 按保存接口带回的离线重生成结果如实提示。未通过校验时总是警告(订阅仍为上一份合法配置);
 *  `appliedText` 是原本「已应用到订阅」的成功文案,给出时才提示成功或「下次刷新后更新」。 */
export function useRegenerateNotice() {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  return useCallback(
    (r: Regenerate | null | undefined, appliedText?: string) => {
      if (r?.status === "invalid") {
        message.warning(t("regenerate.invalid", { count: r.errors?.length ?? 0 }));
      } else if (appliedText) {
        if (r?.status === "pending") message.info(t("regenerate.pending"));
        else message.success(appliedText);
      }
    },
    [message, t],
  );
}
