import { useAppStore } from "../store/useAppStore";
import { t } from "./index";

export function useT(): typeof t {
  useAppStore((state) => state.locale);
  return t;
}
