import { useCallback, useRef, useState } from "react";

import { api } from "../api";

/** 列表页读取:并发 `load` 只采用最后一次发起的结果;失败时保留当前列表并置 `loadFailed`。
 *  排序等乐观更新前调用 `invalidate`,使在途的旧读取作废,不覆盖乐观顺序。 */
export function useLatestList<T>(url: string) {
  const [rows, setRows] = useState<T[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);
  const seq = useRef(0);

  const load = useCallback(async () => {
    const id = ++seq.current;
    try {
      const next = await api<T[]>(url);
      if (id !== seq.current) return;
      setRows(next);
      setLoadFailed(false);
    } catch {
      if (id !== seq.current) return;
      setLoadFailed(true);
    }
    setLoaded(true);
  }, [url]);

  const invalidate = useCallback(() => {
    seq.current += 1;
  }, []);

  return { rows, setRows, loaded, loadFailed, load, invalidate };
}
