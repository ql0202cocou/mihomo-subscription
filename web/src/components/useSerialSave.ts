import { useCallback, useRef } from "react";

/** 排序类保存:按发起顺序串行提交(每次都提交完整顺序,后端按到达顺序生效,不会被较早的请求反超),
 *  并告知调用方本次是否仍是最后一次发起的保存。只有最后一次才更新界面(提示、回滚、重载),
 *  以免较早请求的结果把界面打回旧顺序、或丢掉之后的拖拽。 */
export function useSerialSave() {
  const tail = useRef<Promise<unknown>>(Promise.resolve());
  const seq = useRef(0);
  return useCallback(async <T>(run: () => Promise<T>) => {
    const id = ++seq.current;
    const p = tail.current.then(run);
    tail.current = p.catch(() => undefined);
    try {
      return { ok: true as const, value: await p, latest: id === seq.current };
    } catch (error) {
      return { ok: false as const, error, latest: id === seq.current };
    }
  }, []);
}
