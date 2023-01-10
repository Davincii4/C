use crate::controllers::frontend_prelude::*;

use crate::models::Team;
use crate::schema::teams;
use crate::views::EncodableTeam;

/// Handles the `GET /teams/:team_id` route.
pub async fn show_team(state: State<AppState>, Path(name): Path<String>) -> AppResult<Json<Value>> {
    conduit_compat(move || {
        use self::teams::dsl::{login, teams};

        let conn = state.db_read()?;
        let team: Team = teams.filter(login.eq(&name)).first(&*conn)?;

        Ok(Json(json!({ "team": EncodableTeam::from(team) })))
    })
    .await
}
