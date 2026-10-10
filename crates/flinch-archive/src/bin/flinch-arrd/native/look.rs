//! The last look before a delete, across media servers. The Leaving Soon
//! server (`native.leaving_soon_server`) must answer: without it nothing
//! proves the item unplayed since the evidence was read, so it is kept. The
//! other server, when configured and the item matched there, is read too: a
//! play on either keeps the item.

use super::act::Actor;
use flinch_archive::executor::recheck::{self, Recheck};
use flinch_archive::executor::ShelfServer;

pub(super) async fn last_look(actor: &Actor<'_>, id: &str, baseline: u64) -> Result<(), String> {
    let required = actor.cycle_settings().native.leaving_soon_server;
    let mut looked = false;
    for server in [ShelfServer::Plex, ShelfServer::Jellyfin] {
        let watch = match server {
            ShelfServer::Plex => {
                let (Some(plex), Some((key, _))) = (&actor.plex, actor.plex_item(id)) else {
                    if server == required {
                        return Err("not configured or not matched in Plex, so the last look before deleting cannot be made".into());
                    }
                    continue;
                };
                recheck::read(actor.http(), plex.base, plex.token, &key).await.map_err(|error| format!("the last look failed: {error}"))?
            }
            ShelfServer::Jellyfin => {
                let (Some(jellyfin), Some(item), Some(card)) = (&actor.jellyfin, actor.jellyfin_item(id), actor.cards.get(id)) else {
                    if server == required {
                        return Err(
                            "not configured or not matched in Jellyfin/Emby, so the last look before deleting cannot be made".into()
                        );
                    }
                    continue;
                };
                flinch_archive::jellyfin::last_look(jellyfin.client, item, card.kind)
                    .await
                    .map_err(|error| format!("the last look failed: {error}"))?
            }
        };
        looked = true;
        match recheck::judge(baseline, watch) {
            Recheck::Clear => {}
            verdict => return Err(format!("{verdict} ({})", server.label())),
        }
    }
    if looked {
        Ok(())
    } else {
        Err("no media server could make the last look".into())
    }
}
