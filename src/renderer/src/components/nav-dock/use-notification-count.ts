import { useCallback, useEffect, useRef, useState } from "react";
import { useUserDetails } from "@renderer/hooks";
import { logger } from "@renderer/logger";
import type {
  NotificationCountResponse,
  NotificationsChangedDetail,
} from "@types";

/** Unread local notifications plus the Hydra API's unread count. */
export function useNotificationCount() {
  const { userDetails } = useUserDetails();
  const [notificationCount, setNotificationCount] = useState(0);
  const apiNotificationCountRef = useRef(0);
  const hasFetchedInitialCount = useRef(false);

  const fetchLocalNotificationCount = useCallback(async () => {
    try {
      const localCount = await window.electron.getLocalNotificationsCount();
      setNotificationCount(localCount + apiNotificationCountRef.current);
    } catch (error) {
      logger.error("Failed to fetch local notification count", error);
    }
  }, []);

  const fetchApiNotificationCount = useCallback(async () => {
    try {
      const response =
        await window.electron.hydraApi.get<NotificationCountResponse>(
          "/profile/notifications/count",
          { needsAuth: true }
        );
      apiNotificationCountRef.current = response.count;
    } catch {
      // Ignore API errors
    }
    fetchLocalNotificationCount();
  }, [fetchLocalNotificationCount]);

  useEffect(() => {
    fetchLocalNotificationCount();
  }, [fetchLocalNotificationCount]);

  useEffect(() => {
    if (userDetails && !hasFetchedInitialCount.current) {
      hasFetchedInitialCount.current = true;
      fetchApiNotificationCount();
    } else if (!userDetails) {
      hasFetchedInitialCount.current = false;
      apiNotificationCountRef.current = 0;
      fetchLocalNotificationCount();
    }
  }, [userDetails, fetchApiNotificationCount, fetchLocalNotificationCount]);

  useEffect(() => {
    const unsubscribe = window.electron.onLocalNotificationCreated(() => {
      fetchLocalNotificationCount();
    });
    return () => unsubscribe();
  }, [fetchLocalNotificationCount]);

  useEffect(() => {
    const handleNotificationsChange = (event: Event) => {
      const detail = (event as CustomEvent<NotificationsChangedDetail>).detail;

      if (detail?.resetApiUnread) {
        apiNotificationCountRef.current = 0;
      } else if (typeof detail?.apiUnreadDelta === "number") {
        apiNotificationCountRef.current = Math.max(
          0,
          apiNotificationCountRef.current + detail.apiUnreadDelta
        );
      }

      fetchLocalNotificationCount();
    };
    window.addEventListener("notificationsChanged", handleNotificationsChange);
    return () => {
      window.removeEventListener(
        "notificationsChanged",
        handleNotificationsChange
      );
    };
  }, [fetchLocalNotificationCount]);

  useEffect(() => {
    const unsubscribe = window.electron.onSyncNotificationCount(
      (notification) => {
        apiNotificationCountRef.current = notification.notificationCount;
        fetchLocalNotificationCount();
      }
    );
    return () => unsubscribe();
  }, [fetchLocalNotificationCount]);

  return notificationCount;
}
