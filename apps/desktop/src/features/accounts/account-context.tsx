import { createContext, useContext } from "react";

export const AccountEpochContext = createContext<{ epoch: number; resetAccountData: () => void }>({ epoch: 0, resetAccountData: () => undefined });
export const useAccountEpoch = () => useContext(AccountEpochContext);
