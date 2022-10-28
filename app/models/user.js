import Model, { attr } from '@ember-data/model';
import { inject as service } from '@ember/service';

import { apiAction } from '@mainmatter/ember-api-actions';

export default class User extends Model {
  @service store;

  @attr email;
  @attr email_verified;
  @attr email_verification_sent;
  @attr name;
  @attr login;
  @attr avatar;
  @attr url;
  @attr kind;

  async stats() {
    return await apiAction(this, { method: 'GET', path: 'stats' });
  }

  async changeEmail(email) {
    await apiAction(this, { method: 'PUT', data: { user: { email } } });

    this.store.pushPayload({
      user: {
        id: this.id,
        email,
        email_verified: false,
        email_verification_sent: true,
      },
    });
  }

  async resendVerificationEmail() {
    return await apiAction(this, { method: 'PUT', path: 'resend' });
  }
}
