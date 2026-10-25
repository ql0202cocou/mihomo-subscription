import { useCallback, useRef, useState } from "react";

/** 提交类操作防重入:ref 同步挡住 state 生效前的连续触发(双击只发一次),`pending` 驱动按钮的
 *  loading/confirmLoading。`run` 在前一次完成前再次调用时直接忽略。 */
export function useSingleFlight(): [boolean, (task: () => Promise<void>) => Promise<void>] {
  const [pending, setPending] = useState(false);
  const inFlight = useRef(false);
  const run = useCallback(async (task: () => Promise<void>) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setPending(true);
    try {
      await task();
    } finally {
      inFlight.current = false;
      setPending(false);
    }
  }, []);
  return [pending, run];
}
