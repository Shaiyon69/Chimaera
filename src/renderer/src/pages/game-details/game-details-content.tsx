import { findBundledGame } from "@shared";
import {
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { PencilIcon } from "@primer/octicons-react";
import { useTranslation } from "react-i18next";
import { pickAccent } from "./hero-accent";

import { HeroPanel } from "./hero";
import { DescriptionHeader } from "./description-header/description-header";
import { GallerySlider } from "./gallery-slider/gallery-slider";
import { Sidebar } from "./sidebar/sidebar";
import { GameLogo } from "./game-logo";
import { SimilarGames } from "./similar-games/similar-games";

import { gameDetailsContext } from "@renderer/context";

import tvEffectVideo from "@renderer/assets/emulation/tv-effect.mp4";
import { useAppSelector } from "@renderer/hooks";
import {
  CLASSICS_PS_PLATFORM_LABELS,
  resolveClassicsBadge,
} from "@renderer/helpers";
import {
  EMULATOR_ICONS,
  RETROARCH_EMULATOR_ICON,
} from "@renderer/pages/settings/emulation/emulator-icons";
import "./game-details.scss";

const accentCache = new Map<string, string | null>();

/** Loads the banner small; the GPU downscale is far cheaper than reading it at full size. */
function heroAccent(imageUrl: string): Promise<string | null> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = "";
    img.decoding = "async";
    img.onload = () => {
      const size = 24;
      const canvas = document.createElement("canvas");
      canvas.width = canvas.height = size;
      const context = canvas.getContext("2d", { willReadFrequently: true });
      if (!context) return reject(new Error("no 2d context"));
      context.drawImage(img, 0, 0, size, size);
      resolve(pickAccent(context.getImageData(0, 0, size, size).data));
    };
    img.onerror = () => reject(new Error("banner failed to load"));
    img.src = imageUrl;
  });
}
import "./hero.scss";

const processMediaElements = (document: Document) => {
  const $images = Array.from(document.querySelectorAll("img"));
  $images.forEach(($image) => {
    $image.loading = "lazy";
    $image.removeAttribute("width");
    $image.removeAttribute("height");
    $image.removeAttribute("style");
    $image.style.maxWidth = "100%";
    $image.style.width = "auto";
    $image.style.height = "auto";
    $image.style.boxSizing = "border-box";
  });

  // Handle videos the same way
  const $videos = Array.from(document.querySelectorAll("video"));
  $videos.forEach(($video) => {
    $video.removeAttribute("width");
    $video.removeAttribute("height");
    $video.removeAttribute("style");
    $video.style.maxWidth = "100%";
    $video.style.width = "auto";
    $video.style.height = "auto";
    $video.style.boxSizing = "border-box";
  });
};

const getImageWithCustomPriority = (
  customUrl: string | null | undefined,
  originalUrl: string | null | undefined,
  fallbackUrl?: string | null | undefined
) => {
  return customUrl || originalUrl || fallbackUrl || "";
};

export function GameDetailsContent() {
  const { t } = useTranslation("game_details");

  const {
    objectId,
    shopDetails,
    game,
    hasNSFWContentBlocked,
    shop,
    setShowGameOptionsModal,
    setGameOptionsInitialCategory,
  } = useContext(gameDetailsContext);

  const aboutTheGame = useMemo(() => {
    const aboutTheGame = shopDetails?.about_the_game;
    if (aboutTheGame) {
      const document = new DOMParser().parseFromString(
        aboutTheGame,
        "text/html"
      );

      processMediaElements(document);

      return document.body.outerHTML;
    }

    if (game?.shop === "custom") {
      return "";
    }

    return t("no_shop_details");
  }, [shopDetails, t, game?.shop]);

  // Some store pages (Cyberpunk 2077, for one) are only images with the words baked in. The short
  // description then leads as real text; skipped when the body already opens with it.
  const leadText = useMemo(() => {
    const short = shopDetails?.short_description?.trim();
    if (!short) return null;
    const body = new DOMParser()
      .parseFromString(shopDetails?.about_the_game ?? "", "text/html")
      .body.textContent?.replace(/\s+/g, " ")
      .trim();
    return body?.startsWith(short.slice(0, 60)) ? null : short;
  }, [shopDetails]);

  const [backdropOpacity, setBackdropOpacity] = useState(1);
  // The full store text shows by default; "Show less" folds it for people who want the page short.
  const [isDescriptionExpanded, setIsDescriptionExpanded] = useState(true);
  const [isDescriptionOverflowing, setIsDescriptionOverflowing] =
    useState(false);
  const descriptionRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setBackdropOpacity(1);
  }, [objectId]);

  useLayoutEffect(() => {
    const el = descriptionRef.current;
    if (!el) {
      setIsDescriptionOverflowing(false);
      return;
    }

    const measure = () => {
      const collapsedMaxHeight = 300;
      setIsDescriptionOverflowing(el.scrollHeight > collapsedMaxHeight);
    };

    measure();

    const observer = new ResizeObserver(measure);
    observer.observe(el);

    const images = Array.from(el.querySelectorAll("img"));
    const onMediaLoad = () => measure();
    images.forEach((img) => {
      if (!img.complete) img.addEventListener("load", onMediaLoad);
    });

    return () => {
      observer.disconnect();
      images.forEach((img) => img.removeEventListener("load", onMediaLoad));
    };
  }, [aboutTheGame]);

  const handleEditGameClick = () => {
    setGameOptionsInitialCategory("assets");
    setShowGameOptionsModal(true);
  };

  const userPreferences = useAppSelector(
    (state) => state.userPreferences.value
  );
  const hideClassicsBookmark = userPreferences?.hideClassicsBookmark ?? false;
  const classicsUseHeroLayout = userPreferences?.classicsUseHeroLayout ?? false;

  const isCustomGame = game?.shop === "custom";
  const isLaunchboxGame = shop === "launchbox";
  const renderClassicsHero = isLaunchboxGame && !classicsUseHeroLayout;

  const heroImage = isCustomGame
    ? game?.libraryHeroImageUrl ||
      // A bundled game's library record can predate its art.
      shopDetails?.assets?.libraryHeroImageUrl ||
      game?.iconUrl ||
      ""
    : getImageWithCustomPriority(
        game?.customHeroImageUrl,
        shopDetails?.assets?.libraryHeroImageUrl
      );

  const coverImage =
    game?.customCoverImageUrl ||
    shopDetails?.assets?.coverImageUrl ||
    shopDetails?.assets?.libraryImageUrl ||
    game?.libraryImageUrl ||
    shopDetails?.assets?.iconUrl ||
    game?.iconUrl ||
    "";

  const resolvedHeroImage = isLaunchboxGame
    ? heroImage || coverImage
    : heroImage;

  const [accent, setAccent] = useState<string | null>(null);
  useEffect(() => {
    if (!resolvedHeroImage) return setAccent(null);
    if (accentCache.has(resolvedHeroImage)) {
      return setAccent(accentCache.get(resolvedHeroImage) ?? null);
    }
    setAccent(null);
    let cancelled = false;
    heroAccent(resolvedHeroImage)
      .then((color) => {
        accentCache.set(resolvedHeroImage, color);
        if (!cancelled) setAccent(color);
      })
      // An unreadable image keeps the default accent.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [resolvedHeroImage]);

  const shortDescription = useMemo(() => {
    const raw = shopDetails?.short_description;
    if (!raw) return "";
    // Steam returns HTML entities; decode as text, never inject markup.
    return (
      new DOMParser().parseFromString(raw, "text/html").documentElement
        .textContent ?? ""
    );
  }, [shopDetails?.short_description]);

  const launchboxPlatform = isLaunchboxGame
    ? (game?.platform ?? shopDetails?.platform ?? null)
    : null;

  const launchboxTitle = isLaunchboxGame
    ? (game?.title ?? shopDetails?.name ?? "")
    : "";

  const classicsBadge = resolveClassicsBadge(
    "launchbox",
    isLaunchboxGame ? launchboxPlatform : null,
    CLASSICS_PS_PLATFORM_LABELS,
    {
      emulatorIcons: EMULATOR_ICONS,
      retroarchIcon: RETROARCH_EMULATOR_ICON,
    }
  );

  const classicsChipLabel = classicsBadge.label ?? launchboxPlatform;

  const classicsChips =
    isLaunchboxGame && classicsChipLabel ? (
      <div className="game-details__hero-classics-chips">
        <span className="game-details__hero-classics-chip">
          {classicsChipLabel}
        </span>
        {classicsBadge.icon && (
          <span className="game-details__hero-classics-chip game-details__hero-classics-chip--icon">
            <img src={classicsBadge.icon} alt="" />
          </span>
        )}
      </div>
    ) : null;

  const heroImageContent = resolvedHeroImage ? (
    <img
      src={resolvedHeroImage}
      className="game-details__hero-image"
      alt={game?.title}
    />
  ) : (
    <div className="game-details__hero-image game-details__hero-image--placeholder" />
  );

  const heroButtons = (
    <div className="game-details__hero-buttons game-details__hero-buttons--right">
      {game && (
        <button
          type="button"
          className="game-details__edit-custom-game-button"
          onClick={handleEditGameClick}
          title={t("edit_game_modal_button")}
        >
          <PencilIcon size={16} />
        </button>
      )}
    </div>
  );

  return (
    <div
      className={`game-details__wrapper ${hasNSFWContentBlocked ? "game-details__wrapper--blurred" : ""}`}
      style={
        accent
          ? ({ "--hero-accent": accent } as React.CSSProperties)
          : undefined
      }
    >
      {!renderClassicsHero && resolvedHeroImage && (
        <img
          src={resolvedHeroImage}
          alt=""
          aria-hidden="true"
          className="game-details__ambient"
        />
      )}
      <section className="game-details__container">
        <div
          className={`game-details__hero${renderClassicsHero ? " game-details__hero--classics-wrapper" : ""}`}
        >
          {renderClassicsHero ? (
            <>
              <div className="game-details__hero--classics">
                <div className="game-details__hero-classics-backdrop">
                  {coverImage && (
                    <img src={coverImage} alt="" aria-hidden="true" />
                  )}
                  <div className="game-details__hero-classics-backdrop-overlay" />
                </div>
              </div>
              <div className="game-details__hero-classics-content">
                <div className="game-details__hero-classics-cover">
                  {coverImage && <img src={coverImage} alt={game?.title} />}
                </div>
                <div className="game-details__hero-classics-meta">
                  <h1 className="game-details__hero-classics-title">
                    {launchboxTitle}
                  </h1>
                  {classicsChips}
                </div>
              </div>
            </>
          ) : (
            heroImageContent
          )}

          {isLaunchboxGame && !hideClassicsBookmark && (
            <div className="game-details__hero-bookmark" aria-hidden="true">
              <div className="game-details__hero-classics-rainbow">
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--shadow game-details__hero-classics-stripe--orange">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--shadow" />
                </span>
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--shadow game-details__hero-classics-stripe--red">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--shadow" />
                </span>
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--shadow game-details__hero-classics-stripe--yellow">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--shadow" />
                </span>
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--shadow game-details__hero-classics-stripe--green">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--shadow" />
                </span>
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--shadow game-details__hero-classics-stripe--blue">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--shadow" />
                </span>

                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--red">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--rtl game-details__hero-classics-stripe-band--delay-1">
                    <video
                      src={tvEffectVideo}
                      autoPlay
                      muted
                      loop
                      playsInline
                    />
                  </span>
                </span>
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--orange">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--ltr game-details__hero-classics-stripe-band--delay-2">
                    <video
                      src={tvEffectVideo}
                      autoPlay
                      muted
                      loop
                      playsInline
                    />
                  </span>
                </span>
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--yellow">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--rtl game-details__hero-classics-stripe-band--delay-3">
                    <video
                      src={tvEffectVideo}
                      autoPlay
                      muted
                      loop
                      playsInline
                    />
                  </span>
                </span>
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--green">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--ltr game-details__hero-classics-stripe-band--delay-4">
                    <video
                      src={tvEffectVideo}
                      autoPlay
                      muted
                      loop
                      playsInline
                    />
                  </span>
                </span>
                <span className="game-details__hero-classics-stripe game-details__hero-classics-stripe--blue">
                  <span className="game-details__hero-classics-stripe-band game-details__hero-classics-stripe-band--rtl game-details__hero-classics-stripe-band--delay-5">
                    <video
                      src={tvEffectVideo}
                      autoPlay
                      muted
                      loop
                      playsInline
                    />
                  </span>
                </span>
              </div>
            </div>
          )}

          <div
            className="game-details__hero-logo-backdrop"
            style={{ opacity: backdropOpacity }}
          >
            {renderClassicsHero ? (
              <>
                <div className="game-details__hero-content">{heroButtons}</div>

                <div className="game-details__hero-panel">
                  <HeroPanel />
                </div>
              </>
            ) : (
              <div className="game-details__hero-inner">
                {coverImage && (
                  <img
                    src={coverImage}
                    alt=""
                    className="game-details__cover"
                  />
                )}

                <div className="game-details__hero-text">
                  <div className="game-details__hero-standard-meta">
                    <GameLogo game={game} shopDetails={shopDetails} />
                    {classicsChips}
                  </div>

                  <DescriptionHeader />

                  {shortDescription && (
                    <p className="game-details__hero-summary">
                      {shortDescription}
                    </p>
                  )}

                  {heroButtons}

                  <div className="game-details__hero-panel">
                    <HeroPanel />
                  </div>
                </div>
              </div>
            )}
          </div>
        </div>

        <div className="game-details__description-container">
          <div className="game-details__description-content">
            <section className="game-details__section">
              <h2 className="game-details__description-title">{t("media")}</h2>
              <GallerySlider />
            </section>

            {shopDetails?.about_the_game && (
              <h2 className="game-details__description-title">
                {t("about_this_game")}
              </h2>
            )}

            {shopDetails?.about_the_game && leadText && (
              <p className="game-details__description-lead">{leadText}</p>
            )}

            <div
              ref={descriptionRef}
              dangerouslySetInnerHTML={{
                __html: aboutTheGame,
              }}
              className={`game-details__description ${
                isDescriptionExpanded
                  ? "game-details__description--expanded"
                  : isDescriptionOverflowing
                    ? "game-details__description--collapsed"
                    : ""
              }`}
            />

            {aboutTheGame && isDescriptionOverflowing && (
              <button
                type="button"
                className="game-details__description-toggle"
                onClick={() => setIsDescriptionExpanded(!isDescriptionExpanded)}
              >
                {isDescriptionExpanded ? t("show_less") : t("show_more")}
              </button>
            )}

            {shop && objectId && (
              <SimilarGames objectId={objectId} shop={shop} />
            )}
          </div>

          {(shop !== "custom" || findBundledGame({ shop, objectId })) && (
            <Sidebar />
          )}
        </div>
      </section>
    </div>
  );
}
