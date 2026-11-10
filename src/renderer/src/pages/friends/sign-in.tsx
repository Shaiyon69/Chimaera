import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";

import { Button, TextField } from "@renderer/components";
import { useUserDetails } from "@renderer/hooks";
import { signIn, signUp } from "./messapp/api";
import { classifyAuthError } from "./messapp/errors";
import { useMessappUserId } from "./messapp/use-messapp";
import "./friends.scss";

type AuthMode = "sign_in" | "sign_up";

const AUTH_ERROR_KEYS = {
  invalid_credentials: "sign_in_failed",
  already_registered: "already_registered",
  rate_limited: "rate_limited",
  network: "network_error",
  unknown: "action_failed",
} as const;

function AuthForm() {
  const { t } = useTranslation("messapp");
  const [mode, setMode] = useState<AuthMode>("sign_in");
  const [email, setEmail] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const isSignUp = mode === "sign_up";

  const switchMode = () => {
    setMode(isSignUp ? "sign_in" : "sign_up");
    setError(null);
    setNotice(null);
  };

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    setNotice(null);

    try {
      if (isSignUp) {
        const { needsConfirmation } = await signUp(
          email,
          password,
          username.trim()
        );
        if (needsConfirmation) {
          setNotice(t("check_email"));
          setMode("sign_in");
        }
      } else {
        await signIn(email, password);
      }
    } catch (err) {
      // Never surface the raw SDK message; map what we recognize, fall back to one calm string.
      const kind = classifyAuthError(err);
      // "Incorrect email or password" is wrong copy for a failed sign-up.
      setError(
        t(
          isSignUp && kind === "invalid_credentials"
            ? "action_failed"
            : AUTH_ERROR_KEYS[kind]
        )
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="friends__sign-in" onSubmit={submit}>
      <h2 className="friends__title">
        {t(isSignUp ? "sign_up_title" : "sign_in_title")}
      </h2>
      <p className="friends__hint">
        {t(isSignUp ? "sign_up_description" : "sign_in_description")}
      </p>
      {notice && (
        <p className="friends__notice" role="status">
          {notice}
        </p>
      )}
      <TextField
        label={t("email")}
        type="email"
        autoComplete="username"
        value={email}
        onChange={(event) => setEmail(event.target.value)}
      />
      {isSignUp && (
        <TextField
          label={t("username")}
          autoComplete="nickname"
          value={username}
          onChange={(event) => setUsername(event.target.value)}
        />
      )}
      <TextField
        label={t("password")}
        type="password"
        autoComplete={isSignUp ? "new-password" : "current-password"}
        value={password}
        onChange={(event) => setPassword(event.target.value)}
        error={error}
      />
      <Button
        type="submit"
        disabled={busy || !email || !password || (isSignUp && !username.trim())}
      >
        {busy
          ? t(isSignUp ? "signing_up" : "signing_in")
          : t(isSignUp ? "sign_up" : "sign_in")}
      </Button>
      <p className="friends__no-account">
        {t(isSignUp ? "have_account" : "no_account")}{" "}
        <button
          type="button"
          className="friends__link"
          disabled={busy}
          onClick={switchMode}
        >
          {t(isSignUp ? "sign_in" : "sign_up")}
        </button>
      </p>
    </form>
  );
}

/** The MESSAPP sign-in, opened from the profile button; leaves for the profile once signed in. */
export default function SignIn() {
  const navigate = useNavigate();
  const userId = useMessappUserId();
  const { userDetails } = useUserDetails();

  useEffect(() => {
    if (userId)
      navigate(userDetails ? `/profile/${userDetails.id}` : "/", {
        replace: true,
      });
  }, [userId, userDetails, navigate]);

  return <AuthForm />;
}
