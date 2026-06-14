import { ModCardGrid } from "./ModCardGrid";
import { ModCardList } from "./ModCardList";
import { type ModCardProps, useModCardController } from "./useModCardController";

export function ModCard(props: ModCardProps) {
  const view = useModCardController(props);

  if (props.viewMode === "list") return <ModCardList view={view} />;
  return <ModCardGrid view={view} />;
}
